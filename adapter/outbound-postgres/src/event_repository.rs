use anyhow::Context;
use catapulte_domain::entity::email::EmailId;
use catapulte_domain::entity::sender::SenderName;
use catapulte_domain::port::event_repository::{
    EventRecord, EventRepository, EventRepositoryError, ListEventsParams,
};
use sqlx::QueryBuilder;
use sqlx::Row;

use crate::PostgresAdapter;

impl EventRepository for PostgresAdapter {
    async fn list_events(
        &self,
        params: ListEventsParams,
    ) -> Result<Vec<EventRecord>, EventRepositoryError> {
        let mut qb: QueryBuilder<sqlx::Postgres> = QueryBuilder::new(
            "SELECT id, email_id, event_type, payload, sender_name, created_at FROM lifecycle_events WHERE 1=1",
        );
        if let Some(email_id) = params.email_id {
            qb.push(" AND email_id = ");
            qb.push_bind(email_id.as_uuid());
        }
        if let Some(event_type) = params.event_type.as_deref() {
            qb.push(" AND event_type = ");
            qb.push_bind(event_type.to_owned());
        }
        if let Some(after) = params.after_ms {
            qb.push(" AND created_at > ");
            qb.push_bind(after);
        }
        if let Some(before) = params.before_ms {
            qb.push(" AND created_at < ");
            qb.push_bind(before);
        }
        qb.push(" ORDER BY created_at DESC, id DESC LIMIT ");
        qb.push_bind(i64::from(params.limit));
        qb.push(" OFFSET ");
        qb.push_bind(i64::from(params.offset));

        let rows = qb
            .build()
            .fetch_all(self.pool())
            .await
            .context("listing lifecycle events")
            .map_err(|source| EventRepositoryError::Storage { source })?;

        rows.into_iter()
            .map(|row| PostgresAdapter::row_to_event_record(&row))
            .collect::<anyhow::Result<Vec<_>>>()
            .map_err(|source| EventRepositoryError::Storage { source })
    }
}

impl PostgresAdapter {
    fn row_to_event_record(row: &sqlx::postgres::PgRow) -> anyhow::Result<EventRecord> {
        let id: uuid::Uuid = row.try_get("id").context("reading event id")?;
        let email_id_uuid: uuid::Uuid = row.try_get("email_id").context("reading email_id")?;
        let event_type: String = row.try_get("event_type").context("reading event_type")?;
        let payload: Option<sqlx::types::Json<serde_json::Value>> =
            row.try_get("payload").context("reading payload")?;
        let sender_name: Option<String> =
            row.try_get("sender_name").context("reading sender_name")?;
        let created_at_ms: i64 = row.try_get("created_at").context("reading created_at")?;
        Ok(EventRecord {
            id,
            email_id: EmailId::from(email_id_uuid),
            event_type,
            payload: payload.map(|j| j.0),
            sender_name: sender_name.map(SenderName::new),
            created_at_ms,
        })
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
    use catapulte_domain::port::event_repository::{EventRepository, ListEventsParams};

    use crate::PostgresAdapter;

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
        let url = format!("postgres://catapulte:catapulte@127.0.0.1:{port}/catapulte");
        let adapter = PostgresAdapter::connect(&url).await.unwrap();
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

    fn default_params() -> ListEventsParams {
        ListEventsParams {
            email_id: None,
            event_type: None,
            after_ms: None,
            before_ms: None,
            limit: 20,
            offset: 0,
        }
    }

    async fn adapter_with_email(id: EmailId) -> PostgresAdapter {
        let adapter = fresh_adapter().await;
        adapter.save(id, &sample_envelope()).await.unwrap();
        adapter
    }

    #[tokio::test]
    async fn list_events_returns_empty_when_no_events() {
        let adapter = fresh_adapter().await;
        let events = adapter.list_events(default_params()).await.unwrap();
        assert!(events.is_empty());
    }

    #[tokio::test]
    async fn list_events_filters_by_email_id() {
        let id1 = EmailId::default();
        let id2 = EmailId::default();
        let adapter = fresh_adapter().await;
        adapter.save(id1, &sample_envelope()).await.unwrap();
        adapter.save(id2, &sample_envelope()).await.unwrap();
        adapter
            .publish(&LifecycleEvent::Queued {
                id: id1,
                correlation_id: None,
            })
            .await
            .unwrap();
        adapter
            .publish(&LifecycleEvent::Queued {
                id: id2,
                correlation_id: None,
            })
            .await
            .unwrap();

        let events = adapter
            .list_events(ListEventsParams {
                email_id: Some(id1),
                ..default_params()
            })
            .await
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].email_id, id1);
    }

    #[tokio::test]
    async fn list_events_filters_by_event_type() {
        let id = EmailId::default();
        let adapter = adapter_with_email(id).await;
        adapter
            .publish(&LifecycleEvent::Queued {
                id,
                correlation_id: None,
            })
            .await
            .unwrap();
        adapter
            .publish(&LifecycleEvent::Sent {
                id,
                sender_name: SenderName::new("test"),
                correlation_id: None,
            })
            .await
            .unwrap();

        let events = adapter
            .list_events(ListEventsParams {
                event_type: Some("queued".to_owned()),
                ..default_params()
            })
            .await
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "queued");
    }

    #[tokio::test]
    async fn list_events_respects_limit_and_offset() {
        let id = EmailId::default();
        let adapter = adapter_with_email(id).await;
        for _ in 0..5 {
            adapter
                .publish(&LifecycleEvent::Queued {
                    id,
                    correlation_id: None,
                })
                .await
                .unwrap();
        }

        let page1 = adapter
            .list_events(ListEventsParams {
                limit: 2,
                offset: 0,
                ..default_params()
            })
            .await
            .unwrap();
        assert_eq!(page1.len(), 2);

        let page2 = adapter
            .list_events(ListEventsParams {
                limit: 2,
                offset: 2,
                ..default_params()
            })
            .await
            .unwrap();
        assert_eq!(page2.len(), 2);

        assert_ne!(page1[0].id, page2[0].id);
        assert_ne!(page1[1].id, page2[1].id);
    }

    #[tokio::test]
    async fn list_events_orders_by_created_at_desc() {
        let id = EmailId::default();
        let adapter = adapter_with_email(id).await;
        adapter
            .publish(&LifecycleEvent::Queued {
                id,
                correlation_id: None,
            })
            .await
            .unwrap();
        adapter
            .publish(&LifecycleEvent::Sent {
                id,
                sender_name: SenderName::new("test"),
                correlation_id: None,
            })
            .await
            .unwrap();

        let events = adapter.list_events(default_params()).await.unwrap();
        assert!(events.len() >= 2);
        assert!(events[0].created_at_ms >= events[1].created_at_ms);
    }

    #[tokio::test]
    async fn list_events_filters_by_after_and_before() {
        let id = EmailId::default();
        let adapter = adapter_with_email(id).await;

        sqlx::query(
            "INSERT INTO lifecycle_events (id, email_id, event_type, created_at) VALUES ($1, $2, $3, $4)",
        )
        .bind(uuid::Uuid::now_v7())
        .bind(id.as_uuid())
        .bind("queued")
        .bind(1000i64)
        .execute(adapter.pool())
        .await
        .unwrap();

        sqlx::query(
            "INSERT INTO lifecycle_events (id, email_id, event_type, created_at) VALUES ($1, $2, $3, $4)",
        )
        .bind(uuid::Uuid::now_v7())
        .bind(id.as_uuid())
        .bind("sent")
        .bind(2000i64)
        .execute(adapter.pool())
        .await
        .unwrap();

        let events = adapter
            .list_events(ListEventsParams {
                after_ms: Some(500),
                before_ms: Some(1500),
                ..default_params()
            })
            .await
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "queued");
    }
}
