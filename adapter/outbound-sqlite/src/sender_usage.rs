use anyhow::Context;
use sqlx::{QueryBuilder, Row, Sqlite};

use crate::SqliteAdapter;

impl catapulte_domain::port::sender_usage::SenderUsage for SqliteAdapter {
    async fn get_stats(
        &self,
        names: &[catapulte_domain::entity::sender::SenderName],
        since_ms: i64,
    ) -> Result<
        Vec<catapulte_domain::port::sender_usage::SenderStats>,
        catapulte_domain::port::sender_usage::SenderUsageError,
    > {
        use catapulte_domain::port::sender_usage::{SenderStats, SenderUsageError};

        if names.is_empty() {
            return Ok(vec![]);
        }

        let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new(
            "SELECT sender_name, \
             SUM(CASE WHEN event_type = 'sent' THEN 1 ELSE 0 END) AS sent_count, \
             SUM(CASE WHEN event_type = 'failed' THEN 1 ELSE 0 END) AS failed_count \
             FROM lifecycle_events \
             WHERE event_type IN ('sent', 'failed') \
             AND sender_name IN (",
        );
        let mut sep = qb.separated(", ");
        for name in names {
            sep.push_bind(name.as_str().to_owned());
        }
        qb.push(") AND created_at >= ");
        qb.push_bind(since_ms);
        qb.push(" GROUP BY sender_name");

        let rows = qb
            .build()
            .fetch_all(self.pool())
            .await
            .context("querying sender stats")
            .map_err(|source| SenderUsageError::Storage { source })?;

        let mut map: std::collections::HashMap<String, (u64, u64)> = rows
            .into_iter()
            .map(|row| -> anyhow::Result<(String, (u64, u64))> {
                let name: String = row.try_get("sender_name").context("reading sender_name")?;
                let sent: i64 = row.try_get("sent_count").context("reading sent_count")?;
                let failed: i64 = row
                    .try_get("failed_count")
                    .context("reading failed_count")?;
                Ok((
                    name,
                    (sent.max(0).cast_unsigned(), failed.max(0).cast_unsigned()),
                ))
            })
            .collect::<anyhow::Result<_>>()
            .map_err(|source| SenderUsageError::Storage { source })?;

        Ok(names
            .iter()
            .map(|name| {
                let (sent, failed) = map.remove(name.as_str()).unwrap_or((0, 0));
                SenderStats {
                    name: name.clone(),
                    sent_in_range: sent,
                    failed_in_range: failed,
                }
            })
            .collect())
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
    use catapulte_domain::port::sender_usage::SenderUsage;

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
            attachments: vec![],
        }
    }

    #[tokio::test]
    async fn get_stats_returns_zero_for_sender_with_no_events() {
        let adapter = fresh_adapter().await;
        let names = vec![SenderName::new("primary")];
        let stats = adapter.get_stats(&names, 0).await.unwrap();
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].name, SenderName::new("primary"));
        assert_eq!(stats[0].sent_in_range, 0);
        assert_eq!(stats[0].failed_in_range, 0);
    }

    #[tokio::test]
    async fn get_stats_counts_sent_and_failed_events() {
        let adapter = fresh_adapter().await;
        let id1 = EmailId::default();
        let id2 = EmailId::default();
        adapter.save(id1, &sample_envelope()).await.unwrap();
        adapter.save(id2, &sample_envelope()).await.unwrap();

        adapter
            .publish(&LifecycleEvent::Sent {
                id: id1,
                sender_name: SenderName::new("primary"),
            })
            .await
            .unwrap();
        adapter
            .publish(&LifecycleEvent::Sent {
                id: id2,
                sender_name: SenderName::new("primary"),
            })
            .await
            .unwrap();
        adapter
            .publish(&LifecycleEvent::Failed {
                id: id1,
                attempt: 3,
                reason: "err".to_owned(),
                sender_name: Some(SenderName::new("backup")),
            })
            .await
            .unwrap();

        let names = vec![SenderName::new("primary"), SenderName::new("backup")];
        let stats = adapter.get_stats(&names, 0).await.unwrap();

        let primary = stats
            .iter()
            .find(|s| s.name == SenderName::new("primary"))
            .unwrap();
        let backup = stats
            .iter()
            .find(|s| s.name == SenderName::new("backup"))
            .unwrap();

        assert_eq!(primary.sent_in_range, 2);
        assert_eq!(primary.failed_in_range, 0);
        assert_eq!(backup.sent_in_range, 0);
        assert_eq!(backup.failed_in_range, 1);
    }

    #[tokio::test]
    async fn get_stats_excludes_events_before_since_ms() {
        let adapter = fresh_adapter().await;
        let id = EmailId::default();
        adapter.save(id, &sample_envelope()).await.unwrap();
        adapter
            .publish(&LifecycleEvent::Sent {
                id,
                sender_name: SenderName::new("primary"),
            })
            .await
            .unwrap();

        let names = vec![SenderName::new("primary")];
        let stats = adapter.get_stats(&names, i64::MAX).await.unwrap();
        assert_eq!(stats[0].sent_in_range, 0);
    }

    #[tokio::test]
    async fn get_stats_empty_names_returns_empty() {
        let adapter = fresh_adapter().await;
        let stats = adapter.get_stats(&[], 0).await.unwrap();
        assert!(stats.is_empty());
    }
}
