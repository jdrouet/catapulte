use std::time::Duration;

use anyhow::Context;
use catapulte_domain::entity::body::BodySource;
use catapulte_domain::entity::email::EmailId;
use catapulte_domain::entity::envelope::Envelope;
use catapulte_domain::port::email_queue::{
    AckToken, DequeuedEmail, EmailQueue, EmailQueueError, EnqueueOutcome, TraceCarrier,
};

use crate::PostgresAdapter;
use crate::dto::{EnvelopeBodyDtoDeser, EnvelopeExtrasDto, RecipientDto, recipients_from_dto};

fn now_ms() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(i64::MAX)
}

pub(crate) fn parse_envelope(row: &sqlx::postgres::PgRow) -> anyhow::Result<Envelope> {
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
    let idempotency_key = row
        .try_get("idempotency_key")
        .context("reading idempotency_key")?;
    let correlation_id: Option<String> = row
        .try_get("correlation_id")
        .context("reading correlation_id")?;
    let subject = row.try_get("subject").context("reading subject")?;
    let sender = row.try_get("sender").context("reading sender")?;
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

impl PostgresAdapter {
    /// Returns the count of queue entries that are eligible to be claimed now
    /// (i.e. `claimed_until IS NULL OR claimed_until < now_ms`).
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails.
    pub async fn pending(&self) -> anyhow::Result<u64> {
        use anyhow::Context as _;
        let now = now_ms();
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM email_queue WHERE claimed_until IS NULL OR claimed_until < $1",
        )
        .bind(now)
        .fetch_one(self.pool())
        .await
        .context("counting pending email_queue entries")?;
        Ok(u64::try_from(count).unwrap_or(0))
    }

    #[allow(clippy::too_many_lines)]
    async fn try_dequeue(&self) -> Result<Option<DequeuedEmail>, EmailQueueError> {
        let now = now_ms();

        let mut tx = self
            .pool()
            .begin()
            .await
            .context("beginning transaction")
            .map_err(|source| EmailQueueError::Storage { source })?;

        let maybe = sqlx::query(
            "SELECT id, email_id, attempt_count, trace_context FROM email_queue \
             WHERE claimed_until IS NULL OR claimed_until < $1 \
             ORDER BY enqueued_at ASC LIMIT 1 FOR UPDATE SKIP LOCKED",
        )
        .bind(now)
        .fetch_optional(&mut *tx)
        .await
        .context("finding next queue entry")
        .map_err(|source| EmailQueueError::Storage { source })?;

        let (entry_id, email_id_uuid, current_attempt, trace_raw): (
            uuid::Uuid,
            uuid::Uuid,
            u32,
            Option<String>,
        ) = match maybe {
            None => {
                tx.rollback()
                    .await
                    .context("rolling back empty dequeue")
                    .map_err(|source| EmailQueueError::Storage { source })?;
                return Ok(None);
            }
            Some(row) => {
                use sqlx::Row;
                let entry_id: uuid::Uuid = row
                    .try_get("id")
                    .context("reading entry id")
                    .map_err(|source| EmailQueueError::Storage { source })?;
                let email_id: uuid::Uuid = row
                    .try_get("email_id")
                    .context("reading email_id")
                    .map_err(|source| EmailQueueError::Storage { source })?;
                let attempt: i32 = row
                    .try_get("attempt_count")
                    .context("reading attempt_count")
                    .map_err(|source| EmailQueueError::Storage { source })?;
                let attempt_u32 = u32::try_from(attempt)
                    .context("attempt_count out of range")
                    .map_err(|source| EmailQueueError::Storage { source })?;
                let trace_raw: Option<String> = row
                    .try_get("trace_context")
                    .context("reading trace_context")
                    .map_err(|source| EmailQueueError::Storage { source })?;
                (entry_id, email_id, attempt_u32, trace_raw)
            }
        };

        let new_attempt = current_attempt + 1;
        let processing_timeout_ms: i64 = 300_000;
        let claim_until = now + processing_timeout_ms;

        sqlx::query(
            "UPDATE email_queue SET claimed_until = $1, attempt_count = attempt_count + 1 WHERE id = $2",
        )
        .bind(claim_until)
        .bind(entry_id)
        .execute(&mut *tx)
        .await
        .context("claiming queue entry")
        .map_err(|source| EmailQueueError::Storage { source })?;

        let maybe_row = sqlx::query(
            "SELECT api_key, idempotency_key, correlation_id, subject, sender, recipients, body, variables, extras, tags, send_at_ms FROM emails WHERE id = $1",
        )
        .bind(email_id_uuid)
        .fetch_optional(&mut *tx)
        .await
        .context("fetching email for dequeue")
        .map_err(|source| EmailQueueError::Storage { source })?;

        match maybe_row {
            None => {
                sqlx::query("DELETE FROM email_queue WHERE id = $1")
                    .bind(entry_id)
                    .execute(&mut *tx)
                    .await
                    .context("deleting orphaned queue entry")
                    .map_err(|source| EmailQueueError::Storage { source })?;
                tx.commit()
                    .await
                    .context("committing orphan cleanup transaction")
                    .map_err(|source| EmailQueueError::Storage { source })?;
                Ok(None)
            }
            Some(row) => {
                tx.commit()
                    .await
                    .context("committing dequeue transaction")
                    .map_err(|source| EmailQueueError::Storage { source })?;
                let envelope =
                    parse_envelope(&row).map_err(|source| EmailQueueError::Storage { source })?;
                let token = AckToken::new(entry_id.as_bytes().to_vec());
                let trace = deserialize_trace_context(trace_raw);
                Ok(Some(DequeuedEmail {
                    id: EmailId::from(email_id_uuid),
                    envelope,
                    attempt: new_attempt,
                    token,
                    trace,
                }))
            }
        }
    }
}

impl EmailQueue for PostgresAdapter {
    async fn enqueue(
        &self,
        id: EmailId,
        envelope: &Envelope,
    ) -> Result<EnqueueOutcome, EmailQueueError> {
        let storage = |source| EmailQueueError::Storage { source };
        let entry_id = uuid::Uuid::now_v7();
        let email_id = id.as_uuid();
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
        // The state flip is the once-only decision and the row lock that serialises enqueuers.
        let claimed = sqlx::query(
            "UPDATE emails SET enqueue_state = 'enqueued' \
             WHERE id = $1 AND api_key = $2 AND enqueue_state <> 'enqueued'",
        )
        .bind(email_id)
        .bind(&envelope.owner)
        .execute(&mut *tx)
        .await
        .context("claiming enqueue state")
        .map_err(storage)?
        .rows_affected()
            == 1;
        let outcome = if claimed {
            let has_row: bool =
                sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM email_queue WHERE email_id = $1)")
                    .bind(email_id)
                    .fetch_one(&mut *tx)
                    .await
                    .context("checking for an existing queue row")
                    .map_err(storage)?;
            // A row left by an older version is adopted instead of duplicated.
            if !has_row {
                sqlx::query(
                    "INSERT INTO email_queue (id, email_id, trace_context, claimed_until) \
                     VALUES ($1, $2, $3, $4)",
                )
                .bind(entry_id)
                .bind(email_id)
                .bind(trace_context)
                .bind(envelope.send_at_ms)
                .execute(&mut *tx)
                .await
                .context("inserting into email_queue")
                .map_err(storage)?;
            }
            EnqueueOutcome::Inserted
        } else {
            let exists: Option<String> = sqlx::query_scalar(
                "SELECT enqueue_state FROM emails WHERE id = $1 AND api_key = $2",
            )
            .bind(email_id)
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
        let entry_id = uuid::Uuid::from_slice(&token.0)
            .context("invalid ack token")
            .map_err(|source| EmailQueueError::Storage { source })?;
        sqlx::query("DELETE FROM email_queue WHERE id = $1")
            .bind(entry_id)
            .execute(self.pool())
            .await
            .context("deleting from email_queue")
            .map_err(|source| EmailQueueError::Storage { source })?;
        Ok(())
    }

    async fn nack(&self, token: AckToken, delay: Duration) -> Result<(), EmailQueueError> {
        let now_ms = now_ms();
        let delay_ms = i64::try_from(delay.as_millis()).unwrap_or(i64::MAX);
        let claimed_until = now_ms.saturating_add(delay_ms);
        let entry_id = uuid::Uuid::from_slice(&token.0)
            .context("invalid nack token")
            .map_err(|source| EmailQueueError::Storage { source })?;
        sqlx::query("UPDATE email_queue SET claimed_until = $1 WHERE id = $2")
            .bind(claimed_until)
            .bind(entry_id)
            .execute(self.pool())
            .await
            .context("nacking email_queue entry")
            .map_err(|source| EmailQueueError::Storage { source })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::test_support::fresh_adapter;
    use catapulte_domain::entity::body::{BodySource, Plain};
    use catapulte_domain::entity::email::EmailId;
    use catapulte_domain::entity::envelope::Envelope;
    use catapulte_domain::port::email_queue::{EmailQueue, EmailQueueError, EnqueueOutcome};
    use catapulte_domain::port::email_repository::{
        DuplicateState, EmailRepository, EnqueueState, SaveResult,
    };

    use crate::PostgresAdapter;

    fn sample_envelope() -> Envelope {
        Envelope::new(
            "sender@example.com",
            vec![],
            BodySource::Plain(Plain::try_new(Some("hello".to_owned()), None).unwrap()),
        )
    }

    async fn save_and_enqueue(adapter: &PostgresAdapter, id: EmailId) {
        adapter.save(id, &sample_envelope()).await.unwrap();
        adapter.enqueue(id, &sample_envelope()).await.unwrap();
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
    async fn enqueue_then_dequeue_returns_email() {
        let adapter = fresh_adapter().await;
        let id = EmailId::default();
        save_and_enqueue(&adapter, id).await;

        let dequeued = adapter.dequeue().await.unwrap();
        assert_eq!(dequeued.id, id);
    }

    #[tokio::test]
    async fn ack_removes_item_from_queue() {
        let adapter = fresh_adapter().await;
        let id = EmailId::default();
        save_and_enqueue(&adapter, id).await;

        let dequeued = adapter.dequeue().await.unwrap();
        assert_eq!(dequeued.id, id);

        adapter.ack(dequeued.token).await.unwrap();

        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM email_queue")
            .fetch_one(adapter.pool())
            .await
            .unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn try_dequeue_cleans_up_orphaned_entry_when_email_missing() {
        let adapter = fresh_adapter().await;
        let id = EmailId::default();
        save_and_enqueue(&adapter, id).await;

        // Simulate an orphaned queue entry: delete the queue row first (so the FK
        // allows removing the email), then delete the email, then re-insert a raw
        // queue entry that references a now-missing email_id.
        let entry_id: uuid::Uuid = sqlx::query_scalar("SELECT id FROM email_queue")
            .fetch_one(adapter.pool())
            .await
            .unwrap();
        sqlx::query("DELETE FROM email_queue WHERE id = $1")
            .bind(entry_id)
            .execute(adapter.pool())
            .await
            .unwrap();
        sqlx::query("DELETE FROM emails WHERE id = $1")
            .bind(id.as_uuid())
            .execute(adapter.pool())
            .await
            .unwrap();
        // Re-insert the orphaned queue row on a single connection so the
        // session-level replication-role override and the INSERT share state.
        {
            let mut conn = adapter.pool().acquire().await.unwrap();
            sqlx::query("SET session_replication_role = replica")
                .execute(&mut *conn)
                .await
                .unwrap();
            sqlx::query("INSERT INTO email_queue (id, email_id) VALUES ($1, $2)")
                .bind(entry_id)
                .bind(id.as_uuid())
                .execute(&mut *conn)
                .await
                .unwrap();
            sqlx::query("SET session_replication_role = DEFAULT")
                .execute(&mut *conn)
                .await
                .unwrap();
        }

        // try_dequeue should return None and clean up the orphaned queue entry.
        let result = adapter.try_dequeue().await.unwrap();
        assert!(result.is_none());

        // The queue entry must be gone.
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM email_queue")
            .fetch_one(adapter.pool())
            .await
            .unwrap();
        assert_eq!(count, 0, "orphaned queue entry should have been deleted");
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
    async fn null_trace_context_decodes_to_empty_carrier() {
        let adapter = fresh_adapter().await;
        let id = EmailId::default();
        adapter.save(id, &sample_envelope()).await.unwrap();
        // Insert a queue row with NULL trace_context to simulate a pre-migration row.
        sqlx::query("INSERT INTO email_queue (id, email_id, trace_context) VALUES ($1, $2, NULL)")
            .bind(uuid::Uuid::now_v7())
            .bind(id.as_uuid())
            .execute(adapter.pool())
            .await
            .unwrap();

        let dequeued = adapter.dequeue().await.unwrap();
        assert!(
            dequeued.trace.is_empty(),
            "NULL trace_context must decode to an empty carrier"
        );
    }

    async fn queue_rows(adapter: &PostgresAdapter, id: EmailId) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM email_queue WHERE email_id = $1")
            .bind(id.as_uuid())
            .fetch_one(adapter.pool())
            .await
            .unwrap()
    }

    async fn state_of(adapter: &PostgresAdapter, id: EmailId) -> String {
        sqlx::query_scalar("SELECT enqueue_state FROM emails WHERE id = $1")
            .bind(id.as_uuid())
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
    async fn concurrent_enqueues_insert_one_row() {
        let adapter = fresh_adapter().await;
        let id = EmailId::default();
        adapter.save(id, &sample_envelope()).await.unwrap();
        let envelope = sample_envelope();

        let (a, b, c, d) = tokio::join!(
            adapter.enqueue(id, &envelope),
            adapter.enqueue(id, &envelope),
            adapter.enqueue(id, &envelope),
            adapter.enqueue(id, &envelope),
        );

        let inserted = [a, b, c, d]
            .into_iter()
            .map(Result::unwrap)
            .filter(|o| *o == EnqueueOutcome::Inserted)
            .count();
        assert_eq!(inserted, 1);
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
        sqlx::query("INSERT INTO email_queue (id, email_id) VALUES ($1, $2)")
            .bind(uuid::Uuid::now_v7())
            .bind(id.as_uuid())
            .execute(adapter.pool())
            .await
            .unwrap();

        let outcome = adapter.enqueue(id, &sample_envelope()).await.unwrap();

        assert_eq!(outcome, EnqueueOutcome::Inserted);
        assert_eq!(queue_rows(&adapter, id).await, 1);
        assert_eq!(state_of(&adapter, id).await, "enqueued");
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
        sqlx::query("ALTER TABLE email_queue ADD CONSTRAINT injected CHECK (false) NOT VALID")
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

    #[tokio::test]
    async fn enqueue_waiting_on_a_locked_row_sees_the_committed_state() {
        let adapter = fresh_adapter().await;
        let id = EmailId::default();
        adapter.save(id, &sample_envelope()).await.unwrap();

        let mut tx1 = adapter.pool().begin().await.unwrap();
        let claimed = sqlx::query(
            "UPDATE emails SET enqueue_state = 'enqueued' \
             WHERE id = $1 AND api_key = '' AND enqueue_state <> 'enqueued'",
        )
        .bind(id.as_uuid())
        .execute(&mut *tx1)
        .await
        .unwrap()
        .rows_affected();
        assert_eq!(claimed, 1);

        let task = tokio::spawn({
            let adapter = adapter.clone();
            async move { adapter.enqueue(id, &sample_envelope()).await }
        });
        let mut waiting = false;
        for _ in 0..100 {
            let n: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM pg_stat_activity \
                 WHERE wait_event_type = 'Lock' AND datname = current_database()",
            )
            .fetch_one(adapter.pool())
            .await
            .unwrap();
            if n > 0 {
                waiting = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert!(waiting, "the second enqueue must block on the row lock");
        assert!(!task.is_finished());

        sqlx::query("INSERT INTO email_queue (id, email_id) VALUES ($1, $2)")
            .bind(uuid::Uuid::now_v7())
            .bind(id.as_uuid())
            .execute(&mut *tx1)
            .await
            .unwrap();
        tx1.commit().await.unwrap();

        assert_eq!(
            task.await.unwrap().unwrap(),
            EnqueueOutcome::AlreadyEnqueued
        );
        assert_eq!(queue_rows(&adapter, id).await, 1);
    }
}
