use anyhow::Context;
use catapulte_domain::entity::email::EmailId;
use catapulte_domain::port::event_repository::{
    EventRecord, EventRepository, EventRepositoryError, ListEventsParams,
};
use sqlx::QueryBuilder;
use sqlx::Row;
use sqlx::Sqlite;

use crate::SqliteAdapter;

impl EventRepository for SqliteAdapter {
    async fn list_events(
        &self,
        params: ListEventsParams,
    ) -> Result<Vec<EventRecord>, EventRepositoryError> {
        let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new(
            "SELECT id, email_id, event_type, payload, created_at FROM lifecycle_events WHERE 1=1",
        );
        if let Some(email_id) = params.email_id {
            qb.push(" AND email_id = ");
            qb.push_bind(email_id.as_uuid().as_bytes().to_vec());
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
            .map(|row| SqliteAdapter::row_to_event_record(&row))
            .collect::<anyhow::Result<Vec<_>>>()
            .map_err(|source| EventRepositoryError::Storage { source })
    }
}

impl SqliteAdapter {
    fn row_to_event_record(row: &sqlx::sqlite::SqliteRow) -> anyhow::Result<EventRecord> {
        let id_bytes: Vec<u8> = row.try_get("id").context("reading event id")?;
        let id = uuid::Uuid::from_slice(&id_bytes).context("parsing event id")?;
        let email_id_bytes: Vec<u8> = row.try_get("email_id").context("reading email_id")?;
        let email_id_uuid = uuid::Uuid::from_slice(&email_id_bytes).context("parsing email_id")?;
        let event_type: String = row.try_get("event_type").context("reading event_type")?;
        let payload: Option<sqlx::types::Json<serde_json::Value>> =
            row.try_get("payload").context("reading payload")?;
        let created_at_ms: i64 = row.try_get("created_at").context("reading created_at")?;
        Ok(EventRecord {
            id,
            email_id: EmailId::from(email_id_uuid),
            event_type,
            payload: payload.map(|j| j.0),
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

    async fn adapter_with_email(id: EmailId) -> SqliteAdapter {
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
            .publish(&LifecycleEvent::Queued { id: id1 })
            .await
            .unwrap();
        adapter
            .publish(&LifecycleEvent::Queued { id: id2 })
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
            .publish(&LifecycleEvent::Queued { id })
            .await
            .unwrap();
        adapter
            .publish(&LifecycleEvent::Sent {
                id,
                sender_name: SenderName::new("test"),
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
                .publish(&LifecycleEvent::Queued { id })
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

        // pages should not overlap
        assert_ne!(page1[0].id, page2[0].id);
        assert_ne!(page1[1].id, page2[1].id);
    }

    #[tokio::test]
    async fn list_events_orders_by_created_at_desc() {
        let id = EmailId::default();
        let adapter = adapter_with_email(id).await;
        adapter
            .publish(&LifecycleEvent::Queued { id })
            .await
            .unwrap();
        adapter
            .publish(&LifecycleEvent::Sent {
                id,
                sender_name: SenderName::new("test"),
            })
            .await
            .unwrap();

        let events = adapter.list_events(default_params()).await.unwrap();
        assert!(events.len() >= 2);
        // newest first
        assert!(events[0].created_at_ms >= events[1].created_at_ms);
    }

    #[tokio::test]
    async fn list_events_filters_by_after_and_before() {
        let id = EmailId::default();
        let adapter = adapter_with_email(id).await;

        let event_id1 = uuid::Uuid::now_v7().as_bytes().to_vec();
        let email_id_bytes = id.as_uuid().as_bytes().to_vec();
        sqlx::query(
            "INSERT INTO lifecycle_events (id, email_id, event_type, created_at) VALUES (?, ?, ?, ?)",
        )
        .bind(&event_id1)
        .bind(&email_id_bytes)
        .bind("queued")
        .bind(1000i64)
        .execute(adapter.pool())
        .await
        .unwrap();

        let event_id2 = uuid::Uuid::now_v7().as_bytes().to_vec();
        sqlx::query(
            "INSERT INTO lifecycle_events (id, email_id, event_type, created_at) VALUES (?, ?, ?, ?)",
        )
        .bind(&event_id2)
        .bind(&email_id_bytes)
        .bind("sent")
        .bind(2000i64)
        .execute(adapter.pool())
        .await
        .unwrap();

        // after_ms=500 before_ms=1500 should return only the event at 1000
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
