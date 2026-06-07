use anyhow::Context;
use catapulte_domain::entity::lifecycle_event::LifecycleEvent;
use catapulte_domain::entity::sender::SenderName;
use catapulte_domain::port::event_publisher::{EventPublisher, EventPublisherError};

use crate::PostgresAdapter;

impl EventPublisher for PostgresAdapter {
    /// # Errors
    ///
    /// Returns `EventPublisherError::Publish` when the database insert fails.
    async fn publish(&self, event: &LifecycleEvent) -> Result<(), EventPublisherError> {
        let event_id = uuid::Uuid::now_v7();
        let email_id_uuid = event.email_id().as_uuid();
        // payload is always written as a JSON object so new rows are consistent
        // with the pushed (webhook/NATS) payload. The column remains nullable so
        // rows written before this change keep their NULL value; do not add NOT
        // NULL to the column without a data migration.
        let payload = event.payload();
        let sender_name = event.sender_name().map(SenderName::as_str);
        let error_class = event
            .error_class()
            .map(catapulte_domain::entity::error_class::ErrorClass::as_str);

        sqlx::query(
            "INSERT INTO lifecycle_events (id, email_id, event_type, payload, sender_name, error_class) VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(event_id)
        .bind(email_id_uuid)
        .bind(event.event_type())
        .bind(Some(payload))
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

    use crate::PostgresAdapter;

    async fn wait_for_tcp(port: u16, timeout: std::time::Duration) {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .is_ok()
            {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "127.0.0.1:{port} did not accept connections within {timeout:?}"
            );
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }

    async fn fresh_adapter() -> PostgresAdapter {
        use testcontainers::GenericImage;
        use testcontainers::ImageExt;
        use testcontainers::core::WaitFor;
        use testcontainers::runners::AsyncRunner;

        let pg = GenericImage::new("postgres", "16-alpine")
            .with_wait_for(WaitFor::message_on_stderr(
                "database system is ready to accept connections",
            ))
            .with_env_var("POSTGRES_USER", "catapulte")
            .with_env_var("POSTGRES_PASSWORD", "catapulte")
            .with_env_var("POSTGRES_DB", "catapulte")
            .start()
            .await
            .expect("failed to start postgres container; ensure Docker is running");

        let port = pg.get_host_port_ipv4(5432u16).await.unwrap();
        wait_for_tcp(port, std::time::Duration::from_secs(15)).await;
        let url = format!("postgres://catapulte:catapulte@127.0.0.1:{port}/catapulte");
        let adapter = PostgresAdapter::connect(&url, 10, std::time::Duration::from_secs(30))
            .await
            .unwrap();
        adapter.migrate().await.unwrap();
        std::mem::forget(pg);
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

    async fn adapter_with_email(id: EmailId) -> PostgresAdapter {
        let adapter = fresh_adapter().await;
        adapter.save(id, &sample_envelope()).await.unwrap();
        adapter
    }

    #[tokio::test]
    async fn publish_sent_inserts_row() {
        let id = EmailId::default();
        let adapter = adapter_with_email(id).await;
        adapter
            .publish(&LifecycleEvent::Sent {
                id,
                sender_name: SenderName::new("test"),
                correlation_id: None,
            })
            .await
            .unwrap();

        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM lifecycle_events")
            .fetch_one(adapter.pool())
            .await
            .unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn publish_failed_inserts_row_with_reason() {
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

        let payload: Option<serde_json::Value> =
            sqlx::query_scalar("SELECT payload FROM lifecycle_events")
                .fetch_one(adapter.pool())
                .await
                .unwrap();
        let p = payload.unwrap();
        assert_eq!(p["reason"].as_str(), Some("smtp error"));
        assert_eq!(p["attempt"].as_i64(), Some(3));
        // The stored payload mirrors the pushed (webhook/NATS) payload, so
        // sender_name and error_class are present in it, not only in their columns.
        assert_eq!(p["sender_name"].as_str(), Some("test"));
        assert_eq!(p["error_class"].as_str(), Some("delivery"));
    }

    /// Queued without `correlation_id` now stores `{"correlation_id":null}` — an
    /// object — matching the webhook/NATS wire shape (parity fix).
    #[tokio::test]
    async fn publish_queued_inserts_row_with_correlation_id_payload() {
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

        let payload: Option<serde_json::Value> =
            sqlx::query_scalar("SELECT payload FROM lifecycle_events")
                .fetch_one(adapter.pool())
                .await
                .unwrap();
        let p = payload.unwrap();
        assert_eq!(p, serde_json::json!({ "correlation_id": null }));
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

        let payload: Option<serde_json::Value> =
            sqlx::query_scalar("SELECT payload FROM lifecycle_events")
                .fetch_one(adapter.pool())
                .await
                .unwrap();
        let p = payload.unwrap();
        assert_eq!(p["correlation_id"].as_str(), Some("corr-abc"));
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

        let payload: Option<serde_json::Value> =
            sqlx::query_scalar("SELECT payload FROM lifecycle_events")
                .fetch_one(adapter.pool())
                .await
                .unwrap();
        assert_eq!(
            payload.as_ref().and_then(|v| v["attempt"].as_i64()),
            Some(2)
        );
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

        let payload: Option<serde_json::Value> =
            sqlx::query_scalar("SELECT payload FROM lifecycle_events")
                .fetch_one(adapter.pool())
                .await
                .unwrap();
        let p = payload.unwrap();
        assert_eq!(p["attempt"].as_i64(), Some(1));
        assert_eq!(p["reason"].as_str(), Some("timeout"));
        assert_eq!(p["error_class"].as_str(), Some("delivery"));
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
