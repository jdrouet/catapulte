use std::time::Duration;

use anyhow::Context;
use catapulte_domain::entity::email::EmailId;
use catapulte_domain::entity::envelope::Envelope;
use catapulte_domain::port::email_queue::{
    AckToken, DequeuedEmail, EmailQueue, EmailQueueError, TraceCarrier,
};

use crate::SqliteAdapter;
use crate::dto::{EnvelopeBodyDtoDeser, RecipientDto, recipients_from_dto};

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

fn parse_envelope(row: &sqlx::sqlite::SqliteRow) -> anyhow::Result<Envelope> {
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
    Ok(Envelope {
        idempotency_key,
        correlation_id,
        subject,
        sender,
        recipients: recipients_from_dto(recipients.0),
        body,
        variables: variables.0,
        attachments,
    })
}

fn parse_row(row: &sqlx::sqlite::SqliteRow) -> anyhow::Result<(EmailId, Envelope)> {
    let id = parse_id(row)?;
    let envelope = parse_envelope(row)?;
    Ok((id, envelope))
}

fn now_ms() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(i64::MAX)
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
    /// (i.e. `claimed_until IS NULL OR claimed_until < now_ms`).
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails.
    pub async fn pending(&self) -> anyhow::Result<u64> {
        use anyhow::Context as _;
        let now = now_ms();
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM email_queue WHERE claimed_until IS NULL OR claimed_until < ?",
        )
        .bind(now)
        .fetch_one(self.pool())
        .await
        .context("counting pending email_queue entries")?;
        Ok(u64::try_from(count).unwrap_or(0))
    }

    async fn try_dequeue(&self) -> Result<Option<DequeuedEmail>, EmailQueueError> {
        use sqlx::Row;
        let now = now_ms();
        let processing_timeout_ms: i64 = 300_000;
        let claim_until = now + processing_timeout_ms;

        let maybe = sqlx::query(
            "UPDATE email_queue \
             SET attempt_count = attempt_count + 1, claimed_until = ? \
             WHERE id = ( \
                 SELECT id FROM email_queue \
                 WHERE claimed_until IS NULL OR claimed_until < ? \
                 ORDER BY enqueued_at ASC \
                 LIMIT 1 \
             ) \
             RETURNING id, email_id, attempt_count, trace_context",
        )
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
            "SELECT id, idempotency_key, correlation_id, subject, sender, recipients, body, variables FROM emails WHERE id = ?",
        )
        .bind(&email_id_bytes)
        .fetch_optional(self.pool())
        .await
        .context("fetching email for dequeue")
        .map_err(|source| EmailQueueError::Storage { source })?;

        match maybe_row {
            None => {
                let token = AckToken::new(entry_id_bytes.clone());
                self.ack(token).await?;
                Ok(None)
            }
            Some(row) => parse_row(&row)
                .map(|(id, envelope)| {
                    Some(DequeuedEmail {
                        id,
                        envelope,
                        attempt: new_attempt,
                        token: AckToken::new(entry_id_bytes.clone()),
                        trace,
                    })
                })
                .map_err(|source| EmailQueueError::Storage { source }),
        }
    }
}

impl EmailQueue for SqliteAdapter {
    async fn enqueue(&self, id: EmailId, _envelope: &Envelope) -> Result<(), EmailQueueError> {
        let entry_id = uuid::Uuid::now_v7().as_bytes().to_vec();
        let email_id_bytes = id.as_uuid().as_bytes().to_vec();
        let pairs = catapulte_telemetry::propagation::inject_current();
        let trace_context = if pairs.is_empty() {
            None
        } else {
            Some(
                serde_json::to_string(&pairs)
                    .context("serializing trace context")
                    .map_err(|source| EmailQueueError::Storage { source })?,
            )
        };
        sqlx::query("INSERT INTO email_queue (id, email_id, trace_context) VALUES (?, ?, ?)")
            .bind(entry_id)
            .bind(email_id_bytes)
            .bind(trace_context)
            .execute(self.pool())
            .await
            .context("inserting into email_queue")
            .map_err(|source| EmailQueueError::Storage { source })?;
        Ok(())
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
        let entry_id_bytes = token.0;
        sqlx::query("DELETE FROM email_queue WHERE id = ?")
            .bind(&entry_id_bytes)
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
        let entry_id_bytes = token.0;
        sqlx::query("UPDATE email_queue SET claimed_until = ? WHERE id = ?")
            .bind(claimed_until)
            .bind(&entry_id_bytes)
            .execute(self.pool())
            .await
            .context("nacking email_queue entry")
            .map_err(|source| EmailQueueError::Storage { source })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use catapulte_domain::entity::body::{BodySource, Plain};
    use catapulte_domain::entity::email::EmailId;
    use catapulte_domain::entity::envelope::Envelope;
    use catapulte_domain::port::email_queue::EmailQueue;
    use catapulte_domain::port::email_repository::EmailRepository;

    use crate::SqliteAdapter;

    async fn fresh_adapter() -> SqliteAdapter {
        let adapter = SqliteAdapter::connect(":memory:").await.unwrap();
        adapter.migrate().await.unwrap();
        adapter
    }

    fn sample_envelope() -> Envelope {
        Envelope {
            idempotency_key: None,
            correlation_id: None,
            subject: None,
            sender: "sender@example.com".to_owned(),
            recipients: vec![],
            body: BodySource::Plain(Plain::try_new(Some("hello".to_owned()), None).unwrap()),
            variables: serde_json::Map::new(),
            attachments: vec![],
        }
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
        sqlx::query("INSERT INTO email_queue (id, email_id, trace_context) VALUES (?, ?, NULL)")
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
}
