use std::time::Duration;

use anyhow::Context;
use catapulte_domain::entity::email::EmailId;
use catapulte_domain::entity::envelope::Envelope;
use catapulte_domain::port::email_queue::{
    AckToken, DequeuedEmail, EmailQueue, EmailQueueError, EnqueueOutcome, TraceCarrier,
};

use crate::SqliteAdapter;
use crate::dto::{EnvelopeBodyDtoDeser, EnvelopeExtrasDto, RecipientDto, recipients_from_dto};

use catapulte_domain::entity::body::BodySource;

fn parse_id(row: &sqlx::sqlite::SqliteRow) -> anyhow::Result<EmailId> {
    use sqlx::Row;
    let id_bytes: Vec<u8> = row.try_get("id").context("reading id")?;
    uuid::Uuid::from_slice(&id_bytes)
        .context("invalid id bytes")
        .map(EmailId::from)
}

fn parse_scalars(
    row: &sqlx::sqlite::SqliteRow,
) -> anyhow::Result<(Option<String>, Option<String>, String)> {
    use sqlx::Row;
    let idempotency_key = row
        .try_get("idempotency_key")
        .context("reading idempotency_key")?;
    let subject = row.try_get("subject").context("reading subject")?;
    let sender = row.try_get("sender").context("reading sender")?;
    Ok((idempotency_key, subject, sender))
}

pub(crate) fn parse_envelope(row: &sqlx::sqlite::SqliteRow) -> anyhow::Result<Envelope> {
    use sqlx::Row;
    let body_deser: sqlx::types::Json<EnvelopeBodyDtoDeser> =
        row.try_get("body").context("reading body")?;
    let (body_dto, attachment_dtos) = body_deser.0.split();
    let body = BodySource::try_from(body_dto).context("deserializing body")?;
    let attachments = attachment_dtos
        .into_iter()
        .map(catapulte_domain::entity::attachment::AttachmentRef::from)
        .collect();
    let recipients: sqlx::types::Json<Vec<RecipientDto>> =
        row.try_get("recipients").context("reading recipients")?;
    let variables: sqlx::types::Json<serde_json::Map<String, serde_json::Value>> =
        row.try_get("variables").context("reading variables")?;
    let (idempotency_key, subject, sender) = parse_scalars(row)?;
    let correlation_id: Option<String> = row
        .try_get("correlation_id")
        .context("reading correlation_id")?;
    let extras: Option<sqlx::types::Json<EnvelopeExtrasDto>> =
        row.try_get("extras").context("reading extras")?;
    let extras = extras.map(|j| j.0).unwrap_or_default();
    let tags: Option<sqlx::types::Json<Vec<String>>> =
        row.try_get("tags").context("reading tags")?;
    let owner: String = row.try_get("api_key").context("reading api_key")?;
    let send_at_ms: Option<i64> = row.try_get("send_at_ms").context("reading send_at_ms")?;
    Ok(Envelope {
        owner,
        idempotency_key,
        correlation_id,
        subject,
        sender,
        recipients: recipients_from_dto(recipients.0),
        body,
        variables: variables.0,
        attachments,
        reply_to: extras.reply_to,
        headers: extras.headers,
        unsubscribe: extras
            .unsubscribe
            .map(catapulte_domain::entity::envelope::Unsubscribe::try_from)
            .transpose()
            .context("parsing unsubscribe")?,
        tags: tags.map(|j| j.0).unwrap_or_default(),
        metadata: extras.metadata,
        send_at_ms,
    })
}

fn parse_row(row: &sqlx::sqlite::SqliteRow) -> anyhow::Result<(EmailId, Envelope)> {
    let id = parse_id(row)?;
    let envelope = parse_envelope(row)?;
    Ok((id, envelope))
}

pub(crate) fn now_ms() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(i64::MAX)
}

/// How long a claim holds a row before another worker may take it.
const CLAIM_LEASE_MS: i64 = 300_000;

/// The token is the queue entry id followed by the attempt count the claim set. That count
/// changes on every claim, so a renewal can tell its own claim from a later one.
fn claim_token(entry_id: &[u8], attempt: u32) -> AckToken {
    let mut bytes = entry_id.to_vec();
    bytes.extend_from_slice(&attempt.to_be_bytes());
    AckToken::new(bytes)
}

fn token_parts(token: &AckToken) -> Result<(&[u8], u32), EmailQueueError> {
    token
        .0
        .split_last_chunk::<4>()
        .map(|(id, attempt)| (id, u32::from_be_bytes(*attempt)))
        .ok_or_else(|| EmailQueueError::Storage {
            source: anyhow::anyhow!("invalid queue token"),
        })
}

/// Claims the row with the earliest availability time that is due.
const CLAIM_SQL: &str = "UPDATE email_queue \
     SET attempt_count = attempt_count + 1, claimed_until = ? \
     WHERE id = ( \
         SELECT id FROM email_queue \
         WHERE claimed_until <= ? \
         ORDER BY claimed_until ASC \
         LIMIT 1 \
     ) \
     RETURNING id, email_id, attempt_count, trace_context";

/// A failed `COMMIT` leaves the outcome unknown; every earlier failure rolled back.
fn commit_error(e: sqlx::Error) -> EmailQueueError {
    EmailQueueError::Uncertain {
        source: anyhow::Error::new(e).context("committing enqueue transaction"),
    }
}

fn deserialize_trace_context(raw: Option<String>) -> TraceCarrier {
    let Some(json) = raw else {
        return TraceCarrier::default();
    };
    serde_json::from_str::<Vec<(String, String)>>(&json)
        .map(TraceCarrier::new)
        .unwrap_or_default()
}

impl SqliteAdapter {
    /// Returns the count of queue entries that are eligible to be claimed now
    /// (i.e. `claimed_until <= now_ms`).
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails.
    pub async fn pending(&self) -> anyhow::Result<u64> {
        use anyhow::Context as _;
        let now = now_ms();
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM email_queue WHERE claimed_until <= ?")
                .bind(now)
                .fetch_one(self.pool())
                .await
                .context("counting pending email_queue entries")?;
        Ok(u64::try_from(count).unwrap_or(0))
    }

    async fn try_dequeue(&self) -> Result<Option<DequeuedEmail>, EmailQueueError> {
        use sqlx::Row;
        let now = now_ms();
        let claim_until = now + CLAIM_LEASE_MS;

        let maybe = sqlx::query(CLAIM_SQL)
            .bind(claim_until)
            .bind(now)
            .fetch_optional(self.pool())
            .await
            .context("claiming next queue entry")
            .map_err(|source| EmailQueueError::Storage { source })?;

        let Some(row) = maybe else { return Ok(None) };
        let entry_id_bytes: Vec<u8> = row
            .try_get("id")
            .context("reading entry id")
            .map_err(|source| EmailQueueError::Storage { source })?;
        let email_id_bytes: Vec<u8> = row
            .try_get("email_id")
            .context("reading email_id")
            .map_err(|source| EmailQueueError::Storage { source })?;
        let attempt: i64 = row
            .try_get("attempt_count")
            .context("reading attempt_count")
            .map_err(|source| EmailQueueError::Storage { source })?;
        let new_attempt = u32::try_from(attempt)
            .context("attempt_count out of range")
            .map_err(|source| EmailQueueError::Storage { source })?;
        let trace_raw: Option<String> = row
            .try_get("trace_context")
            .context("reading trace_context")
            .map_err(|source| EmailQueueError::Storage { source })?;
        let trace = deserialize_trace_context(trace_raw);

        let maybe_row = sqlx::query(
            "SELECT id, api_key, idempotency_key, correlation_id, subject, sender, recipients, body, variables, extras, tags, send_at_ms FROM emails WHERE id = ?",
        )
        .bind(&email_id_bytes)
        .fetch_optional(self.pool())
        .await
        .context("fetching email for dequeue")
        .map_err(|source| EmailQueueError::Storage { source })?;

        match maybe_row {
            None => {
                self.ack(claim_token(&entry_id_bytes, new_attempt)).await?;
                Ok(None)
            }
            Some(row) => parse_row(&row)
                .map(|(id, envelope)| {
                    Some(DequeuedEmail {
                        id,
                        envelope,
                        attempt: new_attempt,
                        token: claim_token(&entry_id_bytes, new_attempt),
                        trace,
                    })
                })
                .map_err(|source| EmailQueueError::Storage { source }),
        }
    }
}

impl EmailQueue for SqliteAdapter {
    async fn enqueue(
        &self,
        id: EmailId,
        envelope: &Envelope,
    ) -> Result<EnqueueOutcome, EmailQueueError> {
        let storage = |source| EmailQueueError::Storage { source };
        let entry_id = uuid::Uuid::now_v7().as_bytes().to_vec();
        let email_id_bytes = id.as_uuid().as_bytes().to_vec();
        let pairs = catapulte_telemetry::propagation::inject_current();
        let trace_context = if pairs.is_empty() {
            None
        } else {
            Some(
                serde_json::to_string(&pairs)
                    .context("serializing trace context")
                    .map_err(storage)?,
            )
        };

        let mut tx = self
            .pool()
            .begin()
            .await
            .context("beginning enqueue transaction")
            .map_err(storage)?;
        // The state flip is the once-only decision and the lock that serialises enqueuers.
        let claimed = sqlx::query(
            "UPDATE emails SET enqueue_state = 'enqueued' \
             WHERE id = ? AND api_key = ? AND enqueue_state <> 'enqueued' \
             AND cancelled_at_ms IS NULL",
        )
        .bind(&email_id_bytes)
        .bind(&envelope.owner)
        .execute(&mut *tx)
        .await
        .context("claiming enqueue state")
        .map_err(storage)?
        .rows_affected()
            == 1;
        let outcome = if claimed {
            let has_row: bool =
                sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM email_queue WHERE email_id = ?)")
                    .bind(&email_id_bytes)
                    .fetch_one(&mut *tx)
                    .await
                    .context("checking for an existing queue row")
                    .map_err(storage)?;
            // A row left by an older version is adopted instead of duplicated.
            if !has_row {
                sqlx::query(
                    "INSERT INTO email_queue (id, email_id, trace_context, claimed_until) \
                     VALUES (?, ?, ?, ?)",
                )
                .bind(entry_id)
                .bind(&email_id_bytes)
                .bind(trace_context)
                .bind(envelope.send_at_ms.unwrap_or_else(now_ms))
                .execute(&mut *tx)
                .await
                .context("inserting into email_queue")
                .map_err(storage)?;
            }
            EnqueueOutcome::Inserted
        } else {
            let exists: Option<String> =
                sqlx::query_scalar("SELECT enqueue_state FROM emails WHERE id = ? AND api_key = ?")
                    .bind(&email_id_bytes)
                    .bind(&envelope.owner)
                    .fetch_optional(&mut *tx)
                    .await
                    .context("checking the email row")
                    .map_err(storage)?;
            if exists.is_none() {
                return Err(storage(anyhow::anyhow!(
                    "email not found for owner: id={}",
                    id.as_uuid()
                )));
            }
            EnqueueOutcome::AlreadyEnqueued
        };
        tx.commit().await.map_err(commit_error)?;
        Ok(outcome)
    }

    async fn dequeue(&self) -> Result<DequeuedEmail, EmailQueueError> {
        loop {
            if let Some(item) = self.try_dequeue().await? {
                return Ok(item);
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }

    async fn ack(&self, token: AckToken) -> Result<(), EmailQueueError> {
        let (entry_id_bytes, _) = token_parts(&token)?;
        sqlx::query("DELETE FROM email_queue WHERE id = ?")
            .bind(entry_id_bytes)
            .execute(self.pool())
            .await
            .context("deleting from email_queue")
            .map_err(|source| EmailQueueError::Storage { source })?;
        Ok(())
    }

    async fn nack(&self, token: AckToken, delay: Duration) -> Result<(), EmailQueueError> {
        let now = now_ms();
        let delay_ms = i64::try_from(delay.as_millis()).unwrap_or(i64::MAX);
        let claimed_until = now.saturating_add(delay_ms);
        let (entry_id_bytes, _) = token_parts(&token)?;
        sqlx::query("UPDATE email_queue SET claimed_until = ? WHERE id = ?")
            .bind(claimed_until)
            .bind(entry_id_bytes)
            .execute(self.pool())
            .await
            .context("nacking email_queue entry")
            .map_err(|source| EmailQueueError::Storage { source })?;
        Ok(())
    }

    async fn in_progress(&self, token: &AckToken) -> Result<(), EmailQueueError> {
        let (entry_id, attempt) = token_parts(token)?;
        let renewed = sqlx::query(
            "UPDATE email_queue SET claimed_until = ? WHERE id = ? AND attempt_count = ?",
        )
        .bind(now_ms().saturating_add(CLAIM_LEASE_MS))
        .bind(entry_id)
        .bind(i64::from(attempt))
        .execute(self.pool())
        .await
        .context("extending the email_queue claim")
        .map_err(|source| EmailQueueError::Storage { source })?
        .rows_affected();
        if renewed == 1 {
            Ok(())
        } else {
            Err(EmailQueueError::Storage {
                source: anyhow::anyhow!("the claim is no longer held"),
            })
        }
    }

    fn renew_interval(&self) -> Duration {
        Duration::from_millis(CLAIM_LEASE_MS.unsigned_abs() / 3)
    }
}

#[cfg(test)]
mod tests {
    use catapulte_domain::entity::body::{BodySource, Plain};
    use catapulte_domain::entity::email::EmailId;
    use catapulte_domain::entity::envelope::Envelope;
    use catapulte_domain::port::email_queue::{EmailQueue, EmailQueueError, EnqueueOutcome};
    use catapulte_domain::port::email_repository::{
        CancelOutcome, DuplicateState, EmailCancellation, EmailRepository, EmailStatus,
        EnqueueState, ListEmailsParams, SaveResult,
    };

    use crate::SqliteAdapter;

    async fn fresh_adapter() -> SqliteAdapter {
        let adapter = SqliteAdapter::connect(":memory:").await.unwrap();
        adapter.migrate().await.unwrap();
        adapter
    }

    fn sample_envelope() -> Envelope {
        Envelope::new(
            "sender@example.com",
            vec![],
            BodySource::Plain(Plain::try_new(Some("hello".to_owned()), None).unwrap()),
        )
    }

    async fn save_and_enqueue(adapter: &SqliteAdapter, id: EmailId) {
        adapter.save(id, &sample_envelope()).await.unwrap();
        adapter.enqueue(id, &sample_envelope()).await.unwrap();
    }

    #[tokio::test]
    async fn try_dequeue_returns_none_when_empty() {
        let adapter = fresh_adapter().await;
        assert!(adapter.try_dequeue().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn dequeue_returns_reply_to_and_headers() {
        let adapter = fresh_adapter().await;
        let id = EmailId::default();
        let envelope = Envelope {
            reply_to: vec!["Support <help@example.com>".to_owned()],
            headers: vec![("X-Order-Id".to_owned(), "42".to_owned())],
            ..sample_envelope()
        };
        adapter.save(id, &envelope).await.unwrap();
        adapter.enqueue(id, &envelope).await.unwrap();

        let dequeued = adapter.dequeue().await.unwrap();

        assert_eq!(dequeued.envelope.reply_to, envelope.reply_to);
        assert_eq!(dequeued.envelope.headers, envelope.headers);
    }

    #[tokio::test]
    async fn dequeue_returns_queued_email() {
        let adapter = fresh_adapter().await;
        let id = EmailId::default();
        save_and_enqueue(&adapter, id).await;

        let dequeued = adapter.dequeue().await.unwrap();
        assert_eq!(dequeued.id, id);
    }

    #[tokio::test]
    async fn enqueue_makes_email_visible_to_dequeue() {
        let adapter = fresh_adapter().await;
        let id = EmailId::default();
        adapter.save(id, &sample_envelope()).await.unwrap();
        adapter.enqueue(id, &sample_envelope()).await.unwrap();

        let dequeued = adapter.dequeue().await.unwrap();
        assert_eq!(dequeued.id, id);
    }

    #[tokio::test]
    async fn ack_removes_email_from_queue() {
        let adapter = fresh_adapter().await;
        let id = EmailId::default();
        save_and_enqueue(&adapter, id).await;

        let dequeued = adapter.dequeue().await.unwrap();
        assert_eq!(dequeued.id, id);

        adapter.ack(dequeued.token).await.unwrap();
        assert!(adapter.try_dequeue().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn dequeue_claims_item_so_try_dequeue_returns_none() {
        let adapter = fresh_adapter().await;
        let id = EmailId::default();
        save_and_enqueue(&adapter, id).await;

        let dequeued = adapter.dequeue().await.unwrap();
        assert_eq!(dequeued.id, id);

        assert!(adapter.try_dequeue().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn dequeue_increments_attempt_count() {
        let adapter = fresh_adapter().await;
        let id = EmailId::default();
        save_and_enqueue(&adapter, id).await;

        let dequeued = adapter.dequeue().await.unwrap();
        assert_eq!(dequeued.attempt, 1);

        // Reset claimed_until so the item becomes claimable again
        sqlx::query("UPDATE email_queue SET claimed_until = 0 WHERE email_id = ?")
            .bind(id.as_uuid().as_bytes().to_vec())
            .execute(adapter.pool())
            .await
            .unwrap();

        let dequeued2 = adapter.dequeue().await.unwrap();
        assert_eq!(dequeued2.attempt, 2);
    }

    #[tokio::test]
    async fn concurrent_try_dequeue_claims_at_most_one_item() {
        let adapter = fresh_adapter().await;
        let id = EmailId::default();
        save_and_enqueue(&adapter, id).await;

        let (r1, r2) = tokio::join!(adapter.try_dequeue(), adapter.try_dequeue(),);

        let got1 = r1.unwrap();
        let got2 = r2.unwrap();
        let claimed_count = [got1.is_some(), got2.is_some()]
            .iter()
            .filter(|&&b| b)
            .count();
        assert_eq!(
            claimed_count, 1,
            "exactly one concurrent dequeue should claim the item"
        );
    }

    #[tokio::test]
    async fn nack_updates_claimed_until() {
        let adapter = fresh_adapter().await;
        let id = EmailId::default();
        save_and_enqueue(&adapter, id).await;

        let dequeued = adapter.dequeue().await.unwrap();

        adapter
            .nack(dequeued.token, std::time::Duration::from_secs(10))
            .await
            .unwrap();

        // Item is still claimed; try_dequeue should return None immediately
        assert!(adapter.try_dequeue().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn pending_counts_unclaimed_entries() {
        let adapter = fresh_adapter().await;
        assert_eq!(adapter.pending().await.unwrap(), 0);

        let id1 = EmailId::default();
        let id2 = EmailId::default();
        save_and_enqueue(&adapter, id1).await;
        save_and_enqueue(&adapter, id2).await;

        assert_eq!(adapter.pending().await.unwrap(), 2);

        // Claiming one entry reduces the pending count.
        let _dequeued = adapter.dequeue().await.unwrap();
        assert_eq!(adapter.pending().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn null_trace_context_decodes_to_empty_carrier() {
        let adapter = fresh_adapter().await;
        let id = EmailId::default();
        adapter.save(id, &sample_envelope()).await.unwrap();
        // Insert a queue row with NULL trace_context to simulate a pre-migration row.
        sqlx::query(
            "INSERT INTO email_queue (id, email_id, trace_context, claimed_until) VALUES (?, ?, NULL, 0)",
        )
            .bind(uuid::Uuid::now_v7().as_bytes().to_vec())
            .bind(id.as_uuid().as_bytes().to_vec())
            .execute(adapter.pool())
            .await
            .unwrap();

        let dequeued = adapter.dequeue().await.unwrap();
        assert!(
            dequeued.trace.is_empty(),
            "NULL trace_context must decode to an empty carrier"
        );
    }

    async fn queue_rows(adapter: &SqliteAdapter, id: EmailId) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM email_queue WHERE email_id = ?")
            .bind(id.as_uuid().as_bytes().to_vec())
            .fetch_one(adapter.pool())
            .await
            .unwrap()
    }

    async fn state_of(adapter: &SqliteAdapter, id: EmailId) -> String {
        sqlx::query_scalar("SELECT enqueue_state FROM emails WHERE id = ?")
            .bind(id.as_uuid().as_bytes().to_vec())
            .fetch_one(adapter.pool())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn enqueue_inserts_the_row_and_marks_the_email_enqueued() {
        let adapter = fresh_adapter().await;
        let id = EmailId::default();
        adapter.save(id, &sample_envelope()).await.unwrap();

        let outcome = adapter.enqueue(id, &sample_envelope()).await.unwrap();

        assert_eq!(outcome, EnqueueOutcome::Inserted);
        assert_eq!(queue_rows(&adapter, id).await, 1);
        assert_eq!(state_of(&adapter, id).await, "enqueued");
    }

    #[tokio::test]
    async fn second_enqueue_inserts_no_row() {
        let adapter = fresh_adapter().await;
        let id = EmailId::default();
        save_and_enqueue(&adapter, id).await;

        let outcome = adapter.enqueue(id, &sample_envelope()).await.unwrap();

        assert_eq!(outcome, EnqueueOutcome::AlreadyEnqueued);
        assert_eq!(queue_rows(&adapter, id).await, 1);
    }

    #[tokio::test]
    async fn enqueue_adopts_a_legacy_queue_row() {
        let adapter = fresh_adapter().await;
        let id = EmailId::default();
        adapter.save(id, &sample_envelope()).await.unwrap();
        adapter
            .set_enqueue_state(id, "", EnqueueState::Publishable)
            .await
            .unwrap();
        sqlx::query("INSERT INTO email_queue (id, email_id, claimed_until) VALUES (?, ?, 0)")
            .bind(uuid::Uuid::now_v7().as_bytes().to_vec())
            .bind(id.as_uuid().as_bytes().to_vec())
            .execute(adapter.pool())
            .await
            .unwrap();

        let outcome = adapter.enqueue(id, &sample_envelope()).await.unwrap();

        assert_eq!(outcome, EnqueueOutcome::Inserted);
        assert_eq!(queue_rows(&adapter, id).await, 1);
        assert_eq!(state_of(&adapter, id).await, "enqueued");
        assert_eq!(adapter.try_dequeue().await.unwrap().unwrap().id, id);
    }

    #[tokio::test]
    async fn paused_submit_cannot_enqueue_after_a_recovery_was_acked() {
        let adapter = fresh_adapter().await;
        let mut envelope = sample_envelope();
        envelope.idempotency_key = Some("k".to_owned());
        let a = EmailId::default();
        // Submit A saves and becomes publishable, then pauses.
        adapter.save(a, &envelope).await.unwrap();
        adapter
            .set_enqueue_state(a, "", EnqueueState::Publishable)
            .await
            .unwrap();
        // A same-key retry recovers the row and publishes it as B.
        let SaveResult::Duplicate {
            id: b,
            state: DuplicateState::Complete,
            recovery: Some(recovered),
        } = adapter.save(EmailId::default(), &envelope).await.unwrap()
        else {
            panic!("expected a recovery duplicate");
        };
        assert_eq!(b, a);
        adapter.enqueue(b, &recovered).await.unwrap();
        let dequeued = adapter.try_dequeue().await.unwrap().unwrap();
        adapter.ack(dequeued.token).await.unwrap();

        // The paused submit resumes last.
        let outcome = adapter.enqueue(a, &envelope).await.unwrap();

        assert_eq!(outcome, EnqueueOutcome::AlreadyEnqueued);
        assert_eq!(queue_rows(&adapter, a).await, 0);
        assert!(adapter.try_dequeue().await.unwrap().is_none());
        assert_eq!(state_of(&adapter, a).await, "enqueued");
    }

    #[tokio::test]
    async fn late_enqueue_after_a_terminal_event_adds_nothing_and_keeps_blobs_dead() {
        use catapulte_domain::entity::attachment::{AttachmentRef, BlobRef};
        use catapulte_domain::entity::lifecycle_event::{EventContext, LifecycleEvent};
        use catapulte_domain::port::event_publisher::EventPublisher;

        let adapter = fresh_adapter().await;
        let id = EmailId::default();
        save_and_enqueue(&adapter, id).await;
        adapter
            .set_attachments(
                id,
                &[AttachmentRef {
                    content_id: None,
                    filename: "a.pdf".into(),
                    content_type: "application/pdf".into(),
                    size_bytes: 1,
                    blob: BlobRef {
                        backend: "fs".into(),
                        key: "late".into(),
                    },
                }],
            )
            .await
            .unwrap();
        let dequeued = adapter.dequeue().await.unwrap();
        adapter
            .publish(&LifecycleEvent::Sent {
                id,
                sender_name: catapulte_domain::entity::sender::SenderName::new("test"),
                context: EventContext::with_correlation(None),
                smtp_response: None,
                message_id: "<m@example.com>".to_owned(),
            })
            .await
            .unwrap();
        adapter.ack(dequeued.token).await.unwrap();

        let outcome = adapter.enqueue(id, &sample_envelope()).await.unwrap();

        assert_eq!(outcome, EnqueueOutcome::AlreadyEnqueued);
        assert_eq!(queue_rows(&adapter, id).await, 0);
        assert!(
            adapter
                .list_all_attachment_blobs()
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn failed_insert_is_a_storage_error_and_rolls_the_state_back() {
        let adapter = fresh_adapter().await;
        let id = EmailId::default();
        adapter.save(id, &sample_envelope()).await.unwrap();
        sqlx::query(
            "CREATE TRIGGER fail_insert BEFORE INSERT ON email_queue \
             BEGIN SELECT RAISE(ABORT, 'injected'); END",
        )
        .execute(adapter.pool())
        .await
        .unwrap();

        let err = adapter.enqueue(id, &sample_envelope()).await.unwrap_err();

        assert!(matches!(err, EmailQueueError::Storage { .. }), "{err:?}");
        assert_eq!(state_of(&adapter, id).await, "reserved");
        assert_eq!(queue_rows(&adapter, id).await, 0);
    }

    #[test]
    fn a_commit_error_is_uncertain() {
        let err = super::commit_error(sqlx::Error::PoolClosed);
        assert!(matches!(err, EmailQueueError::Uncertain { .. }));
    }

    #[tokio::test]
    async fn send_at_round_trips_through_save_dequeue_and_recovery() {
        let adapter = fresh_adapter().await;
        let id = EmailId::default();
        let mut envelope = sample_envelope();
        envelope.idempotency_key = Some("k".to_owned());
        envelope.send_at_ms = Some(1_000);
        adapter.save(id, &envelope).await.unwrap();
        adapter
            .set_enqueue_state(id, "", EnqueueState::Publishable)
            .await
            .unwrap();

        let SaveResult::Duplicate {
            recovery: Some(recovered),
            ..
        } = adapter.save(EmailId::default(), &envelope).await.unwrap()
        else {
            panic!("expected a recovery duplicate");
        };
        assert_eq!(recovered.send_at_ms, Some(1_000));

        adapter.enqueue(id, &recovered).await.unwrap();
        let dequeued = adapter.try_dequeue().await.unwrap().unwrap();
        assert_eq!(dequeued.envelope.send_at_ms, Some(1_000));
    }

    #[tokio::test]
    async fn a_future_send_at_is_not_dequeued_but_a_past_one_is() {
        let adapter = fresh_adapter().await;
        let future = EmailId::default();
        let past = EmailId::default();
        let mut envelope = sample_envelope();
        envelope.send_at_ms = Some(super::now_ms() + 3_600_000);
        adapter.save(future, &envelope).await.unwrap();
        adapter.enqueue(future, &envelope).await.unwrap();

        assert!(adapter.try_dequeue().await.unwrap().is_none());

        envelope.send_at_ms = Some(super::now_ms() - 1_000);
        adapter.save(past, &envelope).await.unwrap();
        adapter.enqueue(past, &envelope).await.unwrap();

        assert_eq!(adapter.try_dequeue().await.unwrap().unwrap().id, past);
    }

    #[tokio::test]
    async fn enqueue_of_an_unknown_id_is_a_storage_error() {
        let adapter = fresh_adapter().await;

        let err = adapter
            .enqueue(EmailId::default(), &sample_envelope())
            .await
            .unwrap_err();

        assert!(matches!(err, EmailQueueError::Storage { .. }), "{err:?}");
    }

    #[tokio::test]
    async fn enqueue_with_the_wrong_owner_is_a_storage_error() {
        let adapter = fresh_adapter().await;
        let id = EmailId::default();
        adapter.save(id, &sample_envelope()).await.unwrap();
        let mut other = sample_envelope();
        other.owner = "someone-else".to_owned();

        let err = adapter.enqueue(id, &other).await.unwrap_err();

        assert!(matches!(err, EmailQueueError::Storage { .. }), "{err:?}");
        assert_eq!(queue_rows(&adapter, id).await, 0);
    }

    /// Seeds `count` emails, each with a queue row available at `base + n` (n from 1).
    async fn seed_queue(adapter: &SqliteAdapter, count: i64, base: i64) {
        let proto = EmailId::default();
        adapter.save(proto, &sample_envelope()).await.unwrap();
        sqlx::query(
            "WITH RECURSIVE c(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM c WHERE n < ?) \
             INSERT INTO emails (id, sender, recipients, body, variables) \
             SELECT CAST(printf('e%015d', n) AS BLOB), sender, recipients, body, variables \
             FROM c, (SELECT * FROM emails WHERE id = ?)",
        )
        .bind(count)
        .bind(proto.as_uuid().as_bytes().to_vec())
        .execute(adapter.pool())
        .await
        .unwrap();
        sqlx::query(
            "WITH RECURSIVE c(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM c WHERE n < ?) \
             INSERT INTO email_queue (id, email_id, claimed_until) \
             SELECT CAST(printf('q%015d', n) AS BLOB), CAST(printf('e%015d', n) AS BLOB), ? + n FROM c",
        )
        .bind(count)
        .bind(base)
        .execute(adapter.pool())
        .await
        .unwrap();
        sqlx::query("ANALYZE")
            .execute(adapter.pool())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn claim_with_many_future_rows_uses_the_availability_index() {
        use sqlx::Row;
        let adapter = fresh_adapter().await;
        seed_queue(&adapter, 100_000, super::now_ms() + 3_600_000).await;

        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "EXPLAIN QUERY PLAN {}",
            super::CLAIM_SQL
        )))
        .bind(0_i64)
        .bind(super::now_ms())
        .fetch_all(adapter.pool())
        .await
        .unwrap();
        let details: Vec<String> = rows
            .iter()
            .map(|r| r.try_get::<String, _>("detail").unwrap())
            .collect();
        eprintln!("claim plan: {details:?}");
        assert!(
            details
                .iter()
                .any(|d| d.contains("email_queue_claimed_until")),
            "{details:?}"
        );
        assert!(
            details.iter().all(|d| !d.contains("SCAN email_queue")),
            "{details:?}"
        );

        let started = std::time::Instant::now();
        for _ in 0..100 {
            assert!(adapter.try_dequeue().await.unwrap().is_none());
        }
        let elapsed = started.elapsed();
        eprintln!("100 empty claims: {elapsed:?}");
        assert!(elapsed < std::time::Duration::from_secs(5), "{elapsed:?}");
    }

    #[tokio::test]
    async fn an_immediate_row_is_claimed_before_older_future_rows() {
        let adapter = fresh_adapter().await;
        let mut future = sample_envelope();
        future.send_at_ms = Some(super::now_ms() + 3_600_000);
        for _ in 0..3 {
            let id = EmailId::default();
            adapter.save(id, &future).await.unwrap();
            adapter.enqueue(id, &future).await.unwrap();
        }
        let now_id = EmailId::default();
        adapter.save(now_id, &sample_envelope()).await.unwrap();
        adapter.enqueue(now_id, &sample_envelope()).await.unwrap();

        assert_eq!(adapter.try_dequeue().await.unwrap().unwrap().id, now_id);
        assert!(adapter.try_dequeue().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn a_due_backlog_is_claimed_earliest_first() {
        let adapter = fresh_adapter().await;
        seed_queue(&adapter, 10_000, 0).await;
        let earliest: Vec<u8> = sqlx::query_scalar(
            "SELECT email_id FROM email_queue WHERE claimed_until = (SELECT MIN(claimed_until) FROM email_queue)",
        )
        .fetch_one(adapter.pool())
        .await
        .unwrap();

        let first = adapter.try_dequeue().await.unwrap().unwrap();

        assert_eq!(first.id.as_uuid().as_bytes().to_vec(), earliest);
    }

    #[tokio::test]
    async fn the_migration_backfills_null_rows_so_they_are_claimable() {
        let adapter = fresh_adapter().await;
        let id = EmailId::default();
        adapter.save(id, &sample_envelope()).await.unwrap();
        // Simulate a pre-migration table: no trigger yet.
        sqlx::query("DROP TRIGGER email_queue_claimed_until_default")
            .execute(adapter.pool())
            .await
            .unwrap();
        sqlx::query("INSERT INTO email_queue (id, email_id, claimed_until) VALUES (?, ?, NULL)")
            .bind(uuid::Uuid::now_v7().as_bytes().to_vec())
            .bind(id.as_uuid().as_bytes().to_vec())
            .execute(adapter.pool())
            .await
            .unwrap();
        assert!(adapter.try_dequeue().await.unwrap().is_none());

        sqlx::raw_sql(include_str!(
            "../migrations/20261121000002_queue_availability.sql"
        ))
        .execute(adapter.pool())
        .await
        .unwrap();

        let nulls: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM email_queue WHERE claimed_until IS NULL")
                .fetch_one(adapter.pool())
                .await
                .unwrap();
        assert_eq!(nulls, 0);
        assert_eq!(adapter.try_dequeue().await.unwrap().unwrap().id, id);
    }

    #[tokio::test]
    async fn a_zero_delay_nack_makes_the_row_claimable_again() {
        let adapter = fresh_adapter().await;
        let id = EmailId::default();
        save_and_enqueue(&adapter, id).await;
        let dequeued = adapter.dequeue().await.unwrap();

        adapter
            .nack(dequeued.token, std::time::Duration::ZERO)
            .await
            .unwrap();

        assert_eq!(adapter.try_dequeue().await.unwrap().unwrap().attempt, 2);
    }

    #[tokio::test]
    async fn an_insert_with_a_null_claimed_until_is_claimable() {
        let adapter = fresh_adapter().await;
        let id = EmailId::default();
        adapter.save(id, &sample_envelope()).await.unwrap();

        sqlx::query("INSERT INTO email_queue (id, email_id, claimed_until) VALUES (?, ?, NULL)")
            .bind(uuid::Uuid::now_v7().as_bytes().to_vec())
            .bind(id.as_uuid().as_bytes().to_vec())
            .execute(adapter.pool())
            .await
            .unwrap();

        assert_eq!(adapter.try_dequeue().await.unwrap().unwrap().id, id);
    }

    async fn raw_queue_row(adapter: &SqliteAdapter, id: EmailId, attempts: i64) {
        sqlx::query(
            "INSERT INTO email_queue (id, email_id, claimed_until, attempt_count) VALUES (?, ?, ?, ?)",
        )
        .bind(uuid::Uuid::now_v7().as_bytes().to_vec())
        .bind(id.as_uuid().as_bytes().to_vec())
        .bind(super::now_ms() + 3_600_000)
        .bind(attempts)
        .execute(adapter.pool())
        .await
        .unwrap();
    }

    async fn cancelled_at(adapter: &SqliteAdapter, id: EmailId) -> Option<i64> {
        sqlx::query_scalar("SELECT cancelled_at_ms FROM emails WHERE id = ?")
            .bind(id.as_uuid().as_bytes().to_vec())
            .fetch_one(adapter.pool())
            .await
            .unwrap()
    }

    async fn force_cancelled(adapter: &SqliteAdapter, id: EmailId) {
        sqlx::query("UPDATE emails SET cancelled_at_ms = 1 WHERE id = ?")
            .bind(id.as_uuid().as_bytes().to_vec())
            .execute(adapter.pool())
            .await
            .unwrap();
    }

    async fn scheduled_email(adapter: &SqliteAdapter, owner: &str, send_at_ms: i64) -> EmailId {
        let id = EmailId::default();
        let mut envelope = sample_envelope();
        envelope.owner = owner.to_owned();
        envelope.send_at_ms = Some(send_at_ms);
        envelope.correlation_id = Some("c1".to_owned());
        envelope.tags = vec!["t".to_owned()];
        adapter.save(id, &envelope).await.unwrap();
        adapter.enqueue(id, &envelope).await.unwrap();
        id
    }

    fn in_an_hour() -> i64 {
        super::now_ms() + 3_600_000
    }

    fn due_now() -> i64 {
        super::now_ms() - 1_000
    }

    async fn status_of(adapter: &SqliteAdapter, id: EmailId) -> EmailStatus {
        adapter
            .list_emails(ListEmailsParams {
                tag: None,
                owner: None,
                status: None,
                after_ms: None,
                before_ms: None,
                recipient: None,
                template: None,
                id: Some(id),
                limit: 1,
                offset: 0,
            })
            .await
            .unwrap()[0]
            .status
    }

    #[tokio::test]
    async fn cancel_removes_the_queue_row_and_repeats_as_already_cancelled() {
        let adapter = fresh_adapter().await;
        let id = scheduled_email(&adapter, "", in_an_hour()).await;

        let outcome = adapter.cancel(id, Some("")).await.unwrap();

        let CancelOutcome::Cancelled { context } = outcome else {
            panic!("expected Cancelled, got {outcome:?}");
        };
        assert_eq!(context.correlation_id.as_deref(), Some("c1"));
        assert_eq!(context.tags, vec!["t".to_owned()]);
        assert_eq!(queue_rows(&adapter, id).await, 0);
        assert!(cancelled_at(&adapter, id).await.is_some());
        assert_eq!(
            adapter.cancel(id, Some("")).await.unwrap(),
            CancelOutcome::AlreadyCancelled
        );
        assert!(adapter.try_dequeue().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn a_claimed_email_is_not_cancellable_and_nothing_changes() {
        let adapter = fresh_adapter().await;
        let id = scheduled_email(&adapter, "", due_now()).await;
        adapter.try_dequeue().await.unwrap().unwrap();

        let outcome = adapter.cancel(id, Some("")).await.unwrap();

        assert_eq!(outcome, CancelOutcome::NotCancellable);
        assert_eq!(cancelled_at(&adapter, id).await, None);
        assert_eq!(queue_rows(&adapter, id).await, 1);
    }

    #[tokio::test]
    async fn a_delivered_and_acked_email_is_not_cancellable_and_stays_sent() {
        use catapulte_domain::entity::lifecycle_event::{EventContext, LifecycleEvent};
        use catapulte_domain::port::event_publisher::EventPublisher;

        let adapter = fresh_adapter().await;
        let id = scheduled_email(&adapter, "", due_now()).await;
        let dequeued = adapter.try_dequeue().await.unwrap().unwrap();
        adapter
            .publish(&LifecycleEvent::Sent {
                id,
                sender_name: catapulte_domain::entity::sender::SenderName::new("test"),
                context: EventContext::with_correlation(None),
                smtp_response: None,
                message_id: "<m@example.com>".to_owned(),
            })
            .await
            .unwrap();
        adapter.ack(dequeued.token).await.unwrap();

        let outcome = adapter.cancel(id, Some("")).await.unwrap();

        assert_eq!(outcome, CancelOutcome::NotCancellable);
        assert_eq!(status_of(&adapter, id).await, EmailStatus::Sent);
    }

    #[tokio::test]
    async fn unscheduled_and_publishable_emails_are_not_cancellable() {
        let adapter = fresh_adapter().await;
        let plain = EmailId::default();
        save_and_enqueue(&adapter, plain).await;
        let publishable = EmailId::default();
        let mut envelope = sample_envelope();
        envelope.send_at_ms = Some(in_an_hour());
        adapter.save(publishable, &envelope).await.unwrap();
        adapter
            .set_enqueue_state(publishable, "", EnqueueState::Publishable)
            .await
            .unwrap();

        assert_eq!(
            adapter.cancel(plain, Some("")).await.unwrap(),
            CancelOutcome::NotCancellable
        );
        assert_eq!(
            adapter.cancel(publishable, Some("")).await.unwrap(),
            CancelOutcome::NotCancellable
        );
    }

    #[tokio::test]
    async fn cancel_is_scoped_to_the_owner_and_an_admin_sees_every_email() {
        let adapter = fresh_adapter().await;
        let mine = scheduled_email(&adapter, "a", in_an_hour()).await;
        let theirs = scheduled_email(&adapter, "b", in_an_hour()).await;

        assert_eq!(
            adapter.cancel(theirs, Some("a")).await.unwrap(),
            CancelOutcome::NotFound
        );
        assert_eq!(
            adapter.cancel(EmailId::default(), Some("a")).await.unwrap(),
            CancelOutcome::NotFound
        );
        assert!(matches!(
            adapter.cancel(mine, Some("a")).await.unwrap(),
            CancelOutcome::Cancelled { .. }
        ));
        assert!(matches!(
            adapter.cancel(theirs, None).await.unwrap(),
            CancelOutcome::Cancelled { .. }
        ));
    }

    #[tokio::test]
    async fn two_untouched_rows_roll_the_cancel_back() {
        let adapter = fresh_adapter().await;
        let id = scheduled_email(&adapter, "", in_an_hour()).await;
        raw_queue_row(&adapter, id, 0).await;

        let outcome = adapter.cancel(id, Some("")).await.unwrap();

        assert_eq!(outcome, CancelOutcome::NotCancellable);
        assert_eq!(queue_rows(&adapter, id).await, 2);
        assert_eq!(cancelled_at(&adapter, id).await, None);
    }

    #[tokio::test]
    async fn an_untouched_row_next_to_a_claimed_one_is_not_cancellable() {
        let adapter = fresh_adapter().await;
        let id = scheduled_email(&adapter, "", in_an_hour()).await;
        raw_queue_row(&adapter, id, 1).await;

        let outcome = adapter.cancel(id, Some("")).await.unwrap();

        assert_eq!(outcome, CancelOutcome::NotCancellable);
        assert_eq!(queue_rows(&adapter, id).await, 2);
        assert_eq!(cancelled_at(&adapter, id).await, None);
    }

    #[tokio::test]
    async fn stored_worker_events_block_a_cancel_even_with_a_surviving_untouched_row() {
        use catapulte_domain::entity::lifecycle_event::{EventContext, LifecycleEvent};
        use catapulte_domain::port::event_publisher::EventPublisher;

        let adapter = fresh_adapter().await;
        let id = scheduled_email(&adapter, "", in_an_hour()).await;
        adapter
            .publish(&LifecycleEvent::Sending {
                id,
                attempt: 1,
                context: EventContext::with_correlation(None),
            })
            .await
            .unwrap();
        adapter
            .publish(&LifecycleEvent::Sent {
                id,
                sender_name: catapulte_domain::entity::sender::SenderName::new("test"),
                context: EventContext::with_correlation(None),
                smtp_response: None,
                message_id: "<m@example.com>".to_owned(),
            })
            .await
            .unwrap();

        let outcome = adapter.cancel(id, Some("")).await.unwrap();

        assert_eq!(outcome, CancelOutcome::NotCancellable);
        assert_eq!(status_of(&adapter, id).await, EmailStatus::Sent);
        assert_eq!(queue_rows(&adapter, id).await, 1);
    }

    #[tokio::test]
    async fn enqueue_after_a_cancel_inserts_nothing() {
        let adapter = fresh_adapter().await;
        let id = scheduled_email(&adapter, "", in_an_hour()).await;
        adapter.cancel(id, Some("")).await.unwrap();

        let outcome = adapter.enqueue(id, &sample_envelope()).await.unwrap();

        assert_eq!(outcome, EnqueueOutcome::AlreadyEnqueued);
        assert_eq!(queue_rows(&adapter, id).await, 0);
    }

    #[tokio::test]
    async fn a_cancelled_publishable_duplicate_has_no_recovery_envelope() {
        let adapter = fresh_adapter().await;
        let id = EmailId::default();
        let mut envelope = sample_envelope();
        envelope.idempotency_key = Some("k".to_owned());
        envelope.send_at_ms = Some(in_an_hour());
        adapter.save(id, &envelope).await.unwrap();
        adapter
            .set_enqueue_state(id, "", EnqueueState::Publishable)
            .await
            .unwrap();
        force_cancelled(&adapter, id).await;

        let result = adapter.save(EmailId::default(), &envelope).await.unwrap();

        let SaveResult::Duplicate {
            id: existing,
            state,
            recovery,
        } = result
        else {
            panic!("expected a duplicate");
        };
        assert_eq!(existing, id);
        assert_eq!(state, DuplicateState::Complete);
        assert!(recovery.is_none());
    }

    #[tokio::test]
    async fn cancelled_status_and_filter_and_gc_exclusion() {
        use catapulte_domain::entity::attachment::{AttachmentRef, BlobRef};

        let adapter = fresh_adapter().await;
        let id = scheduled_email(&adapter, "", in_an_hour()).await;
        adapter
            .set_attachments(
                id,
                &[AttachmentRef {
                    content_id: None,
                    filename: "a.pdf".into(),
                    content_type: "application/pdf".into(),
                    size_bytes: 1,
                    blob: BlobRef {
                        backend: "fs".into(),
                        key: "sched".into(),
                    },
                }],
            )
            .await
            .unwrap();
        assert_eq!(adapter.list_all_attachment_blobs().await.unwrap().len(), 1);

        adapter.cancel(id, Some("")).await.unwrap();

        assert_eq!(status_of(&adapter, id).await, EmailStatus::Cancelled);
        assert!(
            adapter
                .list_all_attachment_blobs()
                .await
                .unwrap()
                .is_empty()
        );
        let with = |status| {
            let adapter = adapter.clone();
            async move {
                adapter
                    .list_emails(ListEmailsParams {
                        tag: None,
                        owner: None,
                        status: Some(status),
                        after_ms: None,
                        before_ms: None,
                        recipient: None,
                        template: None,
                        id: None,
                        limit: 20,
                        offset: 0,
                    })
                    .await
                    .unwrap()
                    .len()
            }
        };
        assert_eq!(with(EmailStatus::Cancelled).await, 1);
        assert_eq!(with(EmailStatus::Queued).await, 0);
        assert_eq!(with(EmailStatus::Scheduled).await, 0);
    }

    #[tokio::test]
    async fn cancel_and_claim_race_has_exactly_one_winner() {
        let adapter = fresh_adapter().await;
        let (mut cancels, mut claims) = (0, 0);
        for i in 0..50 {
            let id = scheduled_email(&adapter, "", due_now()).await;
            let cancel = |adapter: SqliteAdapter| {
                tokio::spawn(async move { adapter.cancel(id, Some("")).await })
            };
            let claim =
                |adapter: SqliteAdapter| tokio::spawn(async move { adapter.try_dequeue().await });
            // Alternate who starts first so both outcomes are exercised.
            let (cancel_task, claim_task) = if i % 2 == 0 {
                let c = cancel(adapter.clone());
                (c, claim(adapter.clone()))
            } else {
                let d = claim(adapter.clone());
                (cancel(adapter.clone()), d)
            };
            let cancelled = matches!(
                cancel_task.await.unwrap().unwrap(),
                CancelOutcome::Cancelled { .. }
            );
            let claimed = claim_task.await.unwrap().unwrap().is_some();
            assert_ne!(cancelled, claimed, "exactly one side must win");
            if cancelled {
                cancels += 1;
                assert_eq!(queue_rows(&adapter, id).await, 0);
            } else {
                claims += 1;
                assert_eq!(cancelled_at(&adapter, id).await, None);
            }
            // Clear the claimed row so the next iteration starts from an empty queue.
            sqlx::query("DELETE FROM email_queue")
                .execute(adapter.pool())
                .await
                .unwrap();
        }
        eprintln!("race results: cancel won {cancels}, claim won {claims}");
    }

    async fn bare_scheduled_email(adapter: &SqliteAdapter, key: Option<&str>) -> EmailId {
        let id = EmailId::default();
        let mut envelope = sample_envelope();
        envelope.send_at_ms = Some(in_an_hour());
        envelope.idempotency_key = key.map(str::to_owned);
        adapter.save(id, &envelope).await.unwrap();
        adapter
            .set_enqueue_state(id, "", EnqueueState::Enqueued)
            .await
            .unwrap();
        id
    }

    async fn delivery_started_at(adapter: &SqliteAdapter, id: EmailId) -> Option<i64> {
        sqlx::query_scalar("SELECT delivery_started_at_ms FROM emails WHERE id = ?")
            .bind(id.as_uuid().as_bytes().to_vec())
            .fetch_one(adapter.pool())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn start_delivery_is_repeatable_and_keeps_the_first_timestamp() {
        use catapulte_domain::port::email_repository::{DeliveryStart, EmailCancellation};
        let adapter = fresh_adapter().await;
        let id = bare_scheduled_email(&adapter, None).await;

        assert_eq!(
            adapter.start_delivery(id).await.unwrap(),
            DeliveryStart::Proceed
        );
        let first = delivery_started_at(&adapter, id).await;
        assert!(first.is_some());
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        assert_eq!(
            adapter.start_delivery(id).await.unwrap(),
            DeliveryStart::Proceed
        );

        assert_eq!(delivery_started_at(&adapter, id).await, first);
    }

    #[tokio::test]
    async fn start_delivery_proceeds_for_a_missing_email() {
        use catapulte_domain::port::email_repository::{DeliveryStart, EmailCancellation};
        let adapter = fresh_adapter().await;

        assert_eq!(
            adapter.start_delivery(EmailId::default()).await.unwrap(),
            DeliveryStart::Proceed
        );
    }

    #[tokio::test]
    async fn cancel_and_delivery_start_race_has_exactly_one_winner() {
        use catapulte_domain::port::email_repository::{DeliveryStart, EmailCancellation};
        let adapter = fresh_adapter().await;
        let (mut cancels, mut starts) = (0, 0);
        for i in 0..50 {
            let id = scheduled_email(&adapter, "", in_an_hour()).await;
            let cancel = |adapter: SqliteAdapter| {
                tokio::spawn(async move { adapter.cancel(id, Some("")).await })
            };
            let start = |adapter: SqliteAdapter| {
                tokio::spawn(async move { adapter.start_delivery(id).await })
            };
            let (cancel_task, start_task) = if i % 2 == 0 {
                let c = cancel(adapter.clone());
                (c, start(adapter.clone()))
            } else {
                let s = start(adapter.clone());
                (cancel(adapter.clone()), s)
            };
            let cancelled = matches!(
                cancel_task.await.unwrap().unwrap(),
                CancelOutcome::Cancelled { .. }
            );
            let proceeded = start_task.await.unwrap().unwrap() == DeliveryStart::Proceed;
            assert_ne!(cancelled, proceeded, "exactly one side must win");
            if cancelled {
                cancels += 1;
                assert_eq!(delivery_started_at(&adapter, id).await, None);
            } else {
                starts += 1;
                assert_eq!(cancelled_at(&adapter, id).await, None);
            }
        }
        eprintln!("race results: cancel won {cancels}, delivery start won {starts}");
    }

    #[tokio::test]
    async fn a_delivery_fence_is_worker_evidence_for_a_publishable_duplicate() {
        use catapulte_domain::port::email_repository::EmailCancellation;
        let adapter = fresh_adapter().await;
        let id = bare_scheduled_email(&adapter, Some("k")).await;
        adapter
            .set_enqueue_state(id, "", EnqueueState::Publishable)
            .await
            .unwrap();
        adapter.start_delivery(id).await.unwrap();
        let mut envelope = sample_envelope();
        envelope.idempotency_key = Some("k".to_owned());

        let result = adapter.save(EmailId::default(), &envelope).await.unwrap();

        let SaveResult::Duplicate {
            id: existing,
            state,
            recovery,
        } = result
        else {
            panic!("expected a duplicate");
        };
        assert_eq!(existing, id);
        assert_eq!(state, DuplicateState::Complete);
        assert!(recovery.is_none());
        let stored: String = sqlx::query_scalar("SELECT enqueue_state FROM emails WHERE id = ?")
            .bind(id.as_uuid().as_bytes().to_vec())
            .fetch_one(adapter.pool())
            .await
            .unwrap();
        assert_eq!(stored, "enqueued");
    }

    #[tokio::test]
    async fn a_publishable_duplicate_without_a_fence_recovers_with_its_send_at() {
        let adapter = fresh_adapter().await;
        let id = bare_scheduled_email(&adapter, Some("k")).await;
        adapter
            .set_enqueue_state(id, "", EnqueueState::Publishable)
            .await
            .unwrap();
        let mut envelope = sample_envelope();
        envelope.idempotency_key = Some("k".to_owned());

        let result = adapter.save(EmailId::default(), &envelope).await.unwrap();

        let SaveResult::Duplicate { recovery, .. } = result else {
            panic!("expected a duplicate");
        };
        assert!(recovery.unwrap().send_at_ms.is_some());
    }

    async fn claim_state(
        adapter: &SqliteAdapter,
        token: &catapulte_domain::port::email_queue::AckToken,
    ) -> (i64, i64) {
        sqlx::query_as("SELECT claimed_until, attempt_count FROM email_queue WHERE id = ?")
            .bind(super::token_parts(token).unwrap().0)
            .fetch_one(adapter.pool())
            .await
            .unwrap()
    }

    async fn expire_claim(
        adapter: &SqliteAdapter,
        token: &catapulte_domain::port::email_queue::AckToken,
    ) {
        sqlx::query("UPDATE email_queue SET claimed_until = 1 WHERE id = ?")
            .bind(super::token_parts(token).unwrap().0)
            .execute(adapter.pool())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn in_progress_extends_the_claim_and_keeps_the_attempt() {
        let adapter = fresh_adapter().await;
        scheduled_email(&adapter, "", due_now()).await;
        let token = adapter.try_dequeue().await.unwrap().unwrap().token;
        expire_claim(&adapter, &token).await;

        adapter.in_progress(&token).await.unwrap();

        let (claimed_until, attempts) = claim_state(&adapter, &token).await;
        assert!(claimed_until >= super::now_ms() + 299_000);
        assert_eq!(attempts, 1);
        assert_eq!(
            adapter.renew_interval(),
            std::time::Duration::from_secs(100)
        );
    }

    #[tokio::test]
    async fn a_renewed_claim_cannot_be_taken_by_another_worker() {
        let adapter = fresh_adapter().await;
        scheduled_email(&adapter, "", due_now()).await;
        let token = adapter.try_dequeue().await.unwrap().unwrap().token;
        expire_claim(&adapter, &token).await;
        adapter.in_progress(&token).await.unwrap();

        assert!(adapter.try_dequeue().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn a_stale_claim_cannot_renew_a_row_another_worker_took() {
        let adapter = fresh_adapter().await;
        scheduled_email(&adapter, "", due_now()).await;
        let first = adapter.try_dequeue().await.unwrap().unwrap().token;
        expire_claim(&adapter, &first).await;
        let second = adapter.try_dequeue().await.unwrap().unwrap().token;
        let before = claim_state(&adapter, &second).await;

        let result = adapter.in_progress(&first).await;

        assert!(matches!(result, Err(EmailQueueError::Storage { .. })));
        assert_eq!(claim_state(&adapter, &second).await, before);
        adapter.in_progress(&second).await.unwrap();
    }

    #[tokio::test]
    async fn an_acked_claim_cannot_renew() {
        let adapter = fresh_adapter().await;
        scheduled_email(&adapter, "", due_now()).await;
        let token = adapter.try_dequeue().await.unwrap().unwrap().token;
        adapter.ack(token.clone()).await.unwrap();

        let result = adapter.in_progress(&token).await;

        assert!(matches!(result, Err(EmailQueueError::Storage { .. })));
    }

    #[tokio::test]
    async fn an_unrenewed_expired_claim_is_taken_again() {
        let adapter = fresh_adapter().await;
        scheduled_email(&adapter, "", due_now()).await;
        let token = adapter.try_dequeue().await.unwrap().unwrap().token;
        expire_claim(&adapter, &token).await;

        assert_eq!(adapter.try_dequeue().await.unwrap().unwrap().attempt, 2);
    }
}
