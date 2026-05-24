use anyhow::Context;
use catapulte_domain::entity::email::EmailId;
use catapulte_domain::entity::envelope::Envelope;
use catapulte_domain::port::email_queue::{EmailQueue, EmailQueueError};

use crate::SqliteAdapter;
use crate::dto::{BodySourceDto, RecipientDto, recipients_from_dto};

use catapulte_domain::entity::body::BodySource;

const DEQUEUE_SQL: &str = "
SELECT e.id, e.idempotency_key, e.sender, e.recipients, e.body, e.variables
FROM emails e
WHERE EXISTS (
    SELECT 1 FROM lifecycle_events le
    WHERE le.email_id = e.id AND le.event_type = 'queued'
)
AND NOT EXISTS (
    SELECT 1 FROM lifecycle_events le
    WHERE le.email_id = e.id AND le.event_type IN ('sent', 'failed')
)
ORDER BY (
    SELECT le.id FROM lifecycle_events le
    WHERE le.email_id = e.id AND le.event_type = 'queued'
    LIMIT 1
)
LIMIT 1
";

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

fn parse_scalars(row: &sqlx::sqlite::SqliteRow) -> anyhow::Result<(Option<String>, String)> {
    use sqlx::Row;
    let idempotency_key = row
        .try_get("idempotency_key")
        .context("reading idempotency_key")?;
    let sender = row.try_get("sender").context("reading sender")?;
    Ok((idempotency_key, sender))
}

fn parse_envelope(row: &sqlx::sqlite::SqliteRow) -> anyhow::Result<Envelope> {
    use sqlx::Row;
    let body = parse_body(row)?;
    let recipients: sqlx::types::Json<Vec<RecipientDto>> =
        row.try_get("recipients").context("reading recipients")?;
    let variables: sqlx::types::Json<serde_json::Map<String, serde_json::Value>> =
        row.try_get("variables").context("reading variables")?;
    let (idempotency_key, sender) = parse_scalars(row)?;
    Ok(Envelope {
        idempotency_key,
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

impl EmailQueue for SqliteAdapter {
    async fn enqueue(&self, id: EmailId, envelope: &Envelope) -> Result<(), EmailQueueError> {
        use anyhow::Context;
        let _ = envelope;
        let event_id_bytes = uuid::Uuid::now_v7().as_bytes().to_vec();
        let email_id_bytes = id.as_uuid().as_bytes().to_vec();
        sqlx::query(
            "INSERT INTO lifecycle_events (id, email_id, event_type, payload) VALUES (?, ?, 'queued', NULL)",
        )
        .bind(event_id_bytes)
        .bind(email_id_bytes)
        .execute(self.pool())
        .await
        .context("inserting queued event")
        .map_err(|source| EmailQueueError::Storage { source })?;
        Ok(())
    }

    async fn dequeue(&self) -> Result<Option<(EmailId, Envelope)>, EmailQueueError> {
        let maybe_row = sqlx::query(DEQUEUE_SQL)
            .fetch_optional(self.pool())
            .await
            .context("querying email queue")
            .map_err(|source| EmailQueueError::Storage { source })?;

        maybe_row
            .as_ref()
            .map(parse_row)
            .transpose()
            .map_err(|source| EmailQueueError::Storage { source })
    }
}

#[cfg(test)]
mod tests {
    use catapulte_domain::entity::body::{BodySource, Plain};
    use catapulte_domain::entity::email::EmailId;
    use catapulte_domain::entity::envelope::Envelope;
    use catapulte_domain::entity::lifecycle_event::LifecycleEvent;
    use catapulte_domain::port::email_queue::EmailQueue;
    use catapulte_domain::port::email_repository::EmailRepository;
    use catapulte_domain::port::event_publisher::EventPublisher;

    use crate::SqliteAdapter;

    async fn fresh_adapter() -> SqliteAdapter {
        let adapter = SqliteAdapter::connect(":memory:").await.unwrap();
        adapter.migrate().await.unwrap();
        adapter
    }

    fn sample_envelope() -> Envelope {
        Envelope {
            idempotency_key: None,
            sender: "sender@example.com".to_owned(),
            recipients: vec![],
            body: BodySource::Plain(Plain::try_new(Some("hello".to_owned()), None).unwrap()),
            variables: serde_json::Map::new(),
        }
    }

    async fn save_with_queued(adapter: &SqliteAdapter, id: EmailId) {
        adapter.save(id, &sample_envelope()).await.unwrap();
        adapter
            .publish(&LifecycleEvent::Queued { id })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn dequeue_returns_none_when_empty() {
        let adapter = fresh_adapter().await;
        let result = adapter.dequeue().await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn dequeue_returns_queued_email() {
        let adapter = fresh_adapter().await;
        let id = EmailId::default();
        save_with_queued(&adapter, id).await;

        let result = adapter.dequeue().await.unwrap();
        assert!(result.is_some());
        let (returned_id, _) = result.unwrap();
        assert_eq!(returned_id, id);
    }

    #[tokio::test]
    async fn dequeue_skips_sent_email() {
        let adapter = fresh_adapter().await;
        let id = EmailId::default();
        save_with_queued(&adapter, id).await;
        adapter.publish(&LifecycleEvent::Sent { id }).await.unwrap();

        let result = adapter.dequeue().await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn enqueue_makes_email_visible_to_dequeue() {
        let adapter = fresh_adapter().await;
        let id = EmailId::default();
        adapter.save(id, &sample_envelope()).await.unwrap();
        adapter.enqueue(id, &sample_envelope()).await.unwrap();

        let result = adapter.dequeue().await.unwrap();
        assert!(result.is_some());
        let (returned_id, _) = result.unwrap();
        assert_eq!(returned_id, id);
    }
}
