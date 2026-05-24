use anyhow::Context;
use catapulte_domain::entity::email::EmailId;
use catapulte_domain::entity::envelope::Envelope;
use catapulte_domain::port::email_queue::{EmailQueue, EmailQueueError};

use crate::SqliteAdapter;
use crate::dto::{BodySourceDto, RecipientDto, recipients_from_dto};

use catapulte_domain::entity::body::BodySource;

fn parse_id(row: &sqlx::sqlite::SqliteRow) -> anyhow::Result<EmailId> {
    use sqlx::Row;
    let id_bytes: Vec<u8> = row.try_get("id").context("reading id")?;
    uuid::Uuid::from_slice(&id_bytes)
        .context("invalid id bytes")
        .map(EmailId::from)
}

fn parse_body(row: &sqlx::sqlite::SqliteRow) -> anyhow::Result<BodySource> {
    use sqlx::Row;
    let body: sqlx::types::Json<BodySourceDto> = row.try_get("body").context("reading body")?;
    BodySource::try_from(body.0).context("deserializing body")
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
    let body = parse_body(row)?;
    let recipients: sqlx::types::Json<Vec<RecipientDto>> =
        row.try_get("recipients").context("reading recipients")?;
    let variables: sqlx::types::Json<serde_json::Map<String, serde_json::Value>> =
        row.try_get("variables").context("reading variables")?;
    let (idempotency_key, subject, sender) = parse_scalars(row)?;
    Ok(Envelope {
        idempotency_key,
        subject,
        sender,
        recipients: recipients_from_dto(recipients.0),
        body,
        variables: variables.0,
    })
}

fn parse_row(row: &sqlx::sqlite::SqliteRow) -> anyhow::Result<(EmailId, Envelope)> {
    let id = parse_id(row)?;
    let envelope = parse_envelope(row)?;
    Ok((id, envelope))
}

impl SqliteAdapter {
    async fn try_dequeue(&self) -> Result<Option<(EmailId, Envelope)>, EmailQueueError> {
        let now_ms: i64 = i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
        )
        .unwrap_or(i64::MAX);
        let claim_until_ms = now_ms + 5 * 60 * 1000;

        let maybe_email_id: Option<Vec<u8>> = sqlx::query_scalar(
            "UPDATE email_queue
             SET claimed_until = ?
             WHERE id = (
                 SELECT id FROM email_queue
                 WHERE claimed_until IS NULL OR claimed_until < ?
                 ORDER BY enqueued_at
                 LIMIT 1
             )
             RETURNING email_id",
        )
        .bind(claim_until_ms)
        .bind(now_ms)
        .fetch_optional(self.pool())
        .await
        .context("claiming from email_queue")
        .map_err(|source| EmailQueueError::Storage { source })?;

        let Some(email_id_bytes) = maybe_email_id else {
            return Ok(None);
        };

        let email_id = uuid::Uuid::from_slice(&email_id_bytes)
            .context("invalid email_id bytes in email_queue")
            .map(EmailId::from)
            .map_err(|source| EmailQueueError::Storage { source })?;

        let maybe_row = sqlx::query(
            "SELECT id, idempotency_key, subject, sender, recipients, body, variables FROM emails WHERE id = ?",
        )
        .bind(&email_id_bytes)
        .fetch_optional(self.pool())
        .await
        .context("fetching email for dequeue")
        .map_err(|source| EmailQueueError::Storage { source })?;

        match maybe_row {
            None => {
                self.ack(email_id).await?;
                Ok(None)
            }
            Some(row) => parse_row(&row)
                .map(Some)
                .map_err(|source| EmailQueueError::Storage { source }),
        }
    }
}

impl EmailQueue for SqliteAdapter {
    async fn enqueue(&self, id: EmailId, _envelope: &Envelope) -> Result<(), EmailQueueError> {
        let entry_id = uuid::Uuid::now_v7().as_bytes().to_vec();
        let email_id_bytes = id.as_uuid().as_bytes().to_vec();
        sqlx::query("INSERT INTO email_queue (id, email_id) VALUES (?, ?)")
            .bind(entry_id)
            .bind(email_id_bytes)
            .execute(self.pool())
            .await
            .context("inserting into email_queue")
            .map_err(|source| EmailQueueError::Storage { source })?;
        Ok(())
    }

    async fn dequeue(&self) -> Result<(EmailId, Envelope), EmailQueueError> {
        loop {
            if let Some(item) = self.try_dequeue().await? {
                return Ok(item);
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }

    async fn ack(&self, id: EmailId) -> Result<(), EmailQueueError> {
        let email_id_bytes = id.as_uuid().as_bytes().to_vec();
        sqlx::query("DELETE FROM email_queue WHERE email_id = ?")
            .bind(email_id_bytes)
            .execute(self.pool())
            .await
            .context("deleting from email_queue")
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
            subject: None,
            sender: "sender@example.com".to_owned(),
            recipients: vec![],
            body: BodySource::Plain(Plain::try_new(Some("hello".to_owned()), None).unwrap()),
            variables: serde_json::Map::new(),
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

        let (returned_id, _) = adapter.dequeue().await.unwrap();
        assert_eq!(returned_id, id);
    }

    #[tokio::test]
    async fn enqueue_makes_email_visible_to_dequeue() {
        let adapter = fresh_adapter().await;
        let id = EmailId::default();
        adapter.save(id, &sample_envelope()).await.unwrap();
        adapter.enqueue(id, &sample_envelope()).await.unwrap();

        let (returned_id, _) = adapter.dequeue().await.unwrap();
        assert_eq!(returned_id, id);
    }

    #[tokio::test]
    async fn ack_removes_email_from_queue() {
        let adapter = fresh_adapter().await;
        let id = EmailId::default();
        save_and_enqueue(&adapter, id).await;

        let (returned_id, _) = adapter.dequeue().await.unwrap();
        assert_eq!(returned_id, id);

        adapter.ack(id).await.unwrap();
        assert!(adapter.try_dequeue().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn dequeue_claims_item_so_try_dequeue_returns_none() {
        let adapter = fresh_adapter().await;
        let id = EmailId::default();
        save_and_enqueue(&adapter, id).await;

        let (returned_id, _) = adapter.dequeue().await.unwrap();
        assert_eq!(returned_id, id);

        assert!(adapter.try_dequeue().await.unwrap().is_none());
    }
}
