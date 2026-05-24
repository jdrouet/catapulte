use anyhow::Context;
use catapulte_domain::entity::lifecycle_event::LifecycleEvent;
use catapulte_domain::port::event_publisher::{EventPublisher, EventPublisherError};

use crate::SqliteAdapter;

impl EventPublisher for SqliteAdapter {
    /// # Errors
    ///
    /// Returns `EventPublisherError::Publish` when the database insert fails.
    async fn publish(&self, event: &LifecycleEvent) -> Result<(), EventPublisherError> {
        let (email_id_bytes, event_type, payload) = match event {
            LifecycleEvent::Sent { id } => {
                (id.as_uuid().as_bytes().to_vec(), "sent", None::<String>)
            }
            LifecycleEvent::Failed { id, reason } => (
                id.as_uuid().as_bytes().to_vec(),
                "failed",
                Some(reason.clone()),
            ),
        };

        let event_id_bytes = uuid::Uuid::now_v7().as_bytes().to_vec();

        sqlx::query(
            "INSERT INTO lifecycle_events (id, email_id, event_type, payload) VALUES (?, ?, ?, ?)",
        )
        .bind(event_id_bytes)
        .bind(email_id_bytes)
        .bind(event_type)
        .bind(payload)
        .execute(self.pool())
        .await
        .context("inserting lifecycle event")
        .map_err(|source| EventPublisherError::Publish { source })?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use catapulte_domain::entity::body::{BodySource, Plain};
    use catapulte_domain::entity::email::EmailId;
    use catapulte_domain::entity::envelope::Envelope;
    use catapulte_domain::entity::lifecycle_event::LifecycleEvent;
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
            subject: None,
            sender: "sender@example.com".to_owned(),
            recipients: vec![],
            body: BodySource::Plain(Plain::try_new(Some("hello".to_owned()), None).unwrap()),
            variables: serde_json::Map::new(),
        }
    }

    async fn adapter_with_email(id: EmailId) -> SqliteAdapter {
        let adapter = fresh_adapter().await;
        adapter.save(id, &sample_envelope()).await.unwrap();
        adapter
    }

    #[tokio::test]
    async fn publish_failed_inserts_row_with_event_type_failed() {
        let id = EmailId::default();
        let adapter = adapter_with_email(id).await;
        adapter
            .publish(&LifecycleEvent::Failed {
                id,
                reason: "smtp error".to_owned(),
            })
            .await
            .unwrap();

        let event_type: String = sqlx::query_scalar("SELECT event_type FROM lifecycle_events")
            .fetch_one(adapter.pool())
            .await
            .unwrap();
        assert_eq!(event_type, "failed");

        let payload: Option<String> = sqlx::query_scalar("SELECT payload FROM lifecycle_events")
            .fetch_one(adapter.pool())
            .await
            .unwrap();
        assert_eq!(payload.as_deref(), Some("smtp error"));
    }
}
