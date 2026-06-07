use anyhow::Context;
use catapulte_domain::entity::lifecycle_event::LifecycleEvent;
use catapulte_domain::port::event_publisher::{EventPublisher, EventPublisherError};

use crate::SqliteAdapter;

impl EventPublisher for SqliteAdapter {
    /// # Errors
    ///
    /// Returns `EventPublisherError::Publish` when the database insert fails.
    async fn publish(&self, event: &LifecycleEvent) -> Result<(), EventPublisherError> {
        let (email_id_uuid, payload, sender_name, error_class) = match event {
            LifecycleEvent::Queued { id, correlation_id } => (
                id.as_uuid(),
                correlation_id
                    .as_ref()
                    .map(|cid| serde_json::json!({ "correlation_id": cid })),
                None,
                None,
            ),
            LifecycleEvent::Sending {
                id,
                attempt,
                correlation_id,
            } => (
                id.as_uuid(),
                Some(serde_json::json!({ "attempt": attempt, "correlation_id": correlation_id })),
                None,
                None,
            ),
            LifecycleEvent::Sent {
                id,
                sender_name,
                correlation_id,
            } => (
                id.as_uuid(),
                Some(serde_json::json!({
                    "sender_name": sender_name.as_str(),
                    "correlation_id": correlation_id,
                })),
                Some(sender_name.as_str().to_owned()),
                None,
            ),
            LifecycleEvent::Retrying {
                id,
                attempt,
                reason,
                error_class,
                sender_name,
                correlation_id,
            }
            | LifecycleEvent::Failed {
                id,
                attempt,
                reason,
                error_class,
                sender_name,
                correlation_id,
            } => (
                id.as_uuid(),
                Some(serde_json::json!({
                    "attempt": attempt,
                    "reason": reason,
                    "error_class": error_class.as_str(),
                    "sender_name": sender_name.as_ref().map(catapulte_domain::entity::sender::SenderName::as_str),
                    "correlation_id": correlation_id,
                })),
                sender_name.as_ref().map(|s| s.as_str().to_owned()),
                Some(error_class.as_str().to_owned()),
            ),
        };
        let email_id_bytes = email_id_uuid.as_bytes().to_vec();
        let event_id_bytes = uuid::Uuid::now_v7().as_bytes().to_vec();

        sqlx::query(
            "INSERT INTO lifecycle_events (id, email_id, event_type, payload, sender_name, error_class) VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(event_id_bytes)
        .bind(email_id_bytes)
        .bind(event.event_type())
        .bind(payload.as_ref().map(sqlx::types::Json))
        .bind(sender_name)
        .bind(error_class)
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
    use catapulte_domain::entity::sender::SenderName;
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
            correlation_id: None,
            subject: None,
            sender: "sender@example.com".to_owned(),
            recipients: vec![],
            body: BodySource::Plain(Plain::try_new(Some("hello".to_owned()), None).unwrap()),
            variables: serde_json::Map::new(),
            attachments: vec![],
        }
    }

    async fn adapter_with_email(id: EmailId) -> SqliteAdapter {
        let adapter = fresh_adapter().await;
        adapter.save(id, &sample_envelope()).await.unwrap();
        adapter
    }

    #[tokio::test]
    async fn publish_failed_inserts_row_with_event_type_failed() {
        use catapulte_domain::entity::error_class::ErrorClass;

        let id = EmailId::default();
        let adapter = adapter_with_email(id).await;
        adapter
            .publish(&LifecycleEvent::Failed {
                id,
                attempt: 3,
                reason: "smtp error".to_owned(),
                error_class: ErrorClass::Delivery,
                sender_name: Some(SenderName::new("test")),
                correlation_id: None,
            })
            .await
            .unwrap();

        let event_type: String = sqlx::query_scalar("SELECT event_type FROM lifecycle_events")
            .fetch_one(adapter.pool())
            .await
            .unwrap();
        assert_eq!(event_type, "delivery.failed");

        let payload: Option<sqlx::types::Json<serde_json::Value>> =
            sqlx::query_scalar("SELECT payload FROM lifecycle_events")
                .fetch_one(adapter.pool())
                .await
                .unwrap();
        let p = payload.unwrap().0;
        assert_eq!(p["reason"], "smtp error");
        assert_eq!(p["attempt"], 3);
        // The stored payload mirrors the pushed (webhook/NATS) payload, so
        // sender_name and error_class are present in it, not only in their columns.
        assert_eq!(p["sender_name"], "test");
        assert_eq!(p["error_class"], "delivery");
    }

    #[tokio::test]
    async fn publish_queued_inserts_row_with_event_type_queued_and_null_payload() {
        let id = EmailId::default();
        let adapter = adapter_with_email(id).await;
        adapter
            .publish(&LifecycleEvent::Queued {
                id,
                correlation_id: None,
            })
            .await
            .unwrap();

        let event_type: String = sqlx::query_scalar("SELECT event_type FROM lifecycle_events")
            .fetch_one(adapter.pool())
            .await
            .unwrap();
        assert_eq!(event_type, "queued");

        let payload: Option<sqlx::types::Json<serde_json::Value>> =
            sqlx::query_scalar("SELECT payload FROM lifecycle_events")
                .fetch_one(adapter.pool())
                .await
                .unwrap();
        assert!(payload.is_none());
    }

    #[tokio::test]
    async fn queued_with_correlation_id_persists_payload() {
        let id = EmailId::default();
        let adapter = adapter_with_email(id).await;
        adapter
            .publish(&LifecycleEvent::Queued {
                id,
                correlation_id: Some("corr-abc".to_owned()),
            })
            .await
            .unwrap();

        let payload: Option<sqlx::types::Json<serde_json::Value>> =
            sqlx::query_scalar("SELECT payload FROM lifecycle_events")
                .fetch_one(adapter.pool())
                .await
                .unwrap();
        let p = payload.unwrap().0;
        assert_eq!(p["correlation_id"], "corr-abc");
    }

    #[tokio::test]
    async fn publish_sending_includes_attempt_in_payload() {
        let id = EmailId::default();
        let adapter = adapter_with_email(id).await;
        adapter
            .publish(&LifecycleEvent::Sending {
                id,
                attempt: 2,
                correlation_id: None,
            })
            .await
            .unwrap();

        let payload: Option<sqlx::types::Json<serde_json::Value>> =
            sqlx::query_scalar("SELECT payload FROM lifecycle_events")
                .fetch_one(adapter.pool())
                .await
                .unwrap();
        assert_eq!(payload.unwrap().0["attempt"], 2);
    }

    #[tokio::test]
    async fn publish_retrying_includes_attempt_and_reason_in_payload() {
        use catapulte_domain::entity::error_class::ErrorClass;

        let id = EmailId::default();
        let adapter = adapter_with_email(id).await;
        adapter
            .publish(&LifecycleEvent::Retrying {
                id,
                attempt: 1,
                reason: "timeout".to_owned(),
                error_class: ErrorClass::Delivery,
                sender_name: Some(SenderName::new("test")),
                correlation_id: None,
            })
            .await
            .unwrap();

        let payload: Option<sqlx::types::Json<serde_json::Value>> =
            sqlx::query_scalar("SELECT payload FROM lifecycle_events")
                .fetch_one(adapter.pool())
                .await
                .unwrap();
        let p = payload.unwrap().0;
        assert_eq!(p["attempt"], 1);
        assert_eq!(p["reason"], "timeout");
        assert_eq!(p["error_class"], "delivery");
    }

    #[tokio::test]
    async fn publish_failed_persists_error_class_column() {
        use catapulte_domain::entity::error_class::ErrorClass;

        let id = EmailId::default();
        let adapter = adapter_with_email(id).await;
        adapter
            .publish(&LifecycleEvent::Failed {
                id,
                attempt: 2,
                reason: "no route".to_owned(),
                error_class: ErrorClass::Routing,
                sender_name: None,
                correlation_id: None,
            })
            .await
            .unwrap();

        let error_class: Option<String> =
            sqlx::query_scalar("SELECT error_class FROM lifecycle_events")
                .fetch_one(adapter.pool())
                .await
                .unwrap();
        assert_eq!(error_class.as_deref(), Some("routing"));
    }
}
