use anyhow::Context;
use catapulte_domain::entity::sender::SenderName;
use catapulte_domain::port::sender_repository::{
    SenderRepository, SenderRepositoryError, SenderStats,
};
use sqlx::{QueryBuilder, Row};

use crate::PostgresAdapter;

impl SenderRepository for PostgresAdapter {
    async fn get_stats(
        &self,
        names: &[SenderName],
        since_ms: i64,
    ) -> Result<Vec<SenderStats>, SenderRepositoryError> {
        if names.is_empty() {
            return Ok(vec![]);
        }

        let mut qb: QueryBuilder<sqlx::Postgres> = QueryBuilder::new(
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
            .map_err(|source| SenderRepositoryError::Storage { source })?;

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
            .map_err(|source| SenderRepositoryError::Storage { source })?;

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

impl catapulte_domain::port::sender_usage::SenderUsagePort for PostgresAdapter {
    async fn get_stats(
        &self,
        names: &[SenderName],
        since_ms: i64,
    ) -> Result<
        Vec<catapulte_domain::port::sender_usage::SenderStats>,
        catapulte_domain::port::sender_usage::SenderUsageError,
    > {
        use catapulte_domain::port::sender_usage::{
            SenderStats as UsageSenderStats, SenderUsageError,
        };

        if names.is_empty() {
            return Ok(vec![]);
        }

        let mut qb: QueryBuilder<sqlx::Postgres> = QueryBuilder::new(
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
                UsageSenderStats {
                    name: name.clone(),
                    sent_in_range: sent,
                    failed_in_range: failed,
                }
            })
            .collect())
    }
}
