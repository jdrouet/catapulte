use anyhow::Context;
use catapulte_domain::entity::email::EmailId;
use catapulte_domain::entity::suppression::{Suppression, SuppressionReason};
use catapulte_domain::port::suppression_repository::{
    ListSuppressionsParams, SuppressionRepository, SuppressionRepositoryError,
};
use sqlx::{QueryBuilder, Row, Sqlite};

use crate::SqliteAdapter;

fn storage(source: anyhow::Error) -> SuppressionRepositoryError {
    SuppressionRepositoryError::Storage { source }
}

impl SuppressionRepository for SqliteAdapter {
    async fn list(
        &self,
        params: ListSuppressionsParams,
    ) -> Result<Vec<Suppression>, SuppressionRepositoryError> {
        let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new(
            "SELECT address, reason, created_at_ms, source_email_id FROM suppressions WHERE 1=1",
        );
        if let Some(reason) = params.reason {
            qb.push(" AND reason = ");
            qb.push_bind(reason.as_str());
        }
        if let Some(address) = params.address {
            qb.push(" AND address LIKE '%' || ");
            qb.push_bind(address.to_ascii_lowercase());
            qb.push(" || '%'");
        }
        qb.push(" ORDER BY created_at_ms DESC, address LIMIT ");
        qb.push_bind(i64::from(params.limit));
        qb.push(" OFFSET ");
        qb.push_bind(i64::from(params.offset));

        let rows = qb
            .build()
            .fetch_all(self.pool())
            .await
            .context("listing suppressions")
            .map_err(storage)?;
        rows.iter()
            .map(|row| {
                let reason: String = row.try_get("reason").context("reading reason")?;
                let source: Option<Vec<u8>> = row
                    .try_get("source_email_id")
                    .context("reading source_email_id")?;
                Ok(Suppression {
                    address: row.try_get("address").context("reading address")?,
                    reason: reason.parse::<SuppressionReason>()?,
                    created_at_ms: row
                        .try_get("created_at_ms")
                        .context("reading created_at_ms")?,
                    source_email_id: source
                        .map(|b| uuid::Uuid::from_slice(&b).map(EmailId::from))
                        .transpose()
                        .context("parsing source_email_id")?,
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()
            .map_err(storage)
    }

    async fn add(&self, entries: &[Suppression]) -> Result<(), SuppressionRepositoryError> {
        for entry in entries {
            sqlx::query(
                "INSERT OR IGNORE INTO suppressions (address, reason, created_at_ms, source_email_id) \
                 VALUES (?, ?, ?, ?)",
            )
            .bind(&entry.address)
            .bind(entry.reason.as_str())
            .bind(entry.created_at_ms)
            .bind(entry.source_email_id.map(|id| id.as_uuid().as_bytes().to_vec()))
            .execute(self.pool())
            .await
            .context("adding suppression")
            .map_err(storage)?;
        }
        Ok(())
    }

    async fn delete(&self, address: &str) -> Result<bool, SuppressionRepositoryError> {
        let result = sqlx::query("DELETE FROM suppressions WHERE address = ?")
            .bind(address)
            .execute(self.pool())
            .await
            .context("deleting suppression")
            .map_err(storage)?;
        Ok(result.rows_affected() > 0)
    }

    async fn find_suppressed(
        &self,
        addresses: &[String],
    ) -> Result<std::collections::HashSet<String>, SuppressionRepositoryError> {
        if addresses.is_empty() {
            return Ok(std::collections::HashSet::new());
        }
        let mut qb: QueryBuilder<Sqlite> =
            QueryBuilder::new("SELECT address FROM suppressions WHERE address IN (");
        let mut sep = qb.separated(", ");
        for address in addresses {
            sep.push_bind(address);
        }
        qb.push(")");
        let found: Vec<String> = qb
            .build_query_scalar()
            .fetch_all(self.pool())
            .await
            .context("finding suppressed addresses")
            .map_err(storage)?;
        Ok(found.into_iter().collect())
    }
}

#[cfg(test)]
mod tests {
    use catapulte_domain::entity::suppression::{Suppression, SuppressionReason};
    use catapulte_domain::port::suppression_repository::{
        ListSuppressionsParams, SuppressionRepository,
    };

    use crate::SqliteAdapter;

    async fn fresh_adapter() -> SqliteAdapter {
        let adapter = SqliteAdapter::connect(":memory:").await.unwrap();
        adapter.migrate().await.unwrap();
        adapter
    }

    fn entry(address: &str, reason: SuppressionReason, at: i64) -> Suppression {
        Suppression {
            address: address.to_owned(),
            reason,
            created_at_ms: at,
            source_email_id: None,
        }
    }

    fn params() -> ListSuppressionsParams {
        ListSuppressionsParams {
            reason: None,
            address: None,
            limit: 20,
            offset: 0,
        }
    }

    #[tokio::test]
    async fn add_list_filter_and_delete() {
        let adapter = fresh_adapter().await;
        adapter
            .add(&[
                entry("a@x.com", SuppressionReason::Bounce, 1),
                entry("b@x.com", SuppressionReason::Manual, 2),
                entry("a@x.com", SuppressionReason::Complaint, 3),
            ])
            .await
            .unwrap();

        let all = adapter.list(params()).await.unwrap();
        assert_eq!(all.len(), 2, "a repeated address is stored once");
        assert_eq!(all[0].address, "b@x.com", "newest first");
        let first = all.iter().find(|s| s.address == "a@x.com").unwrap();
        assert_eq!(first.reason, SuppressionReason::Bounce, "first entry wins");

        let manual = adapter
            .list(ListSuppressionsParams {
                reason: Some(SuppressionReason::Manual),
                ..params()
            })
            .await
            .unwrap();
        assert_eq!(manual.len(), 1);
        let by_address = adapter
            .list(ListSuppressionsParams {
                address: Some("A@X".to_owned()),
                ..params()
            })
            .await
            .unwrap();
        assert_eq!(by_address.len(), 1);

        assert!(adapter.delete("a@x.com").await.unwrap());
        assert!(!adapter.delete("a@x.com").await.unwrap());
        assert_eq!(adapter.list(params()).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn find_suppressed_returns_only_listed_addresses() {
        let adapter = fresh_adapter().await;
        adapter
            .add(&[entry("a@x.com", SuppressionReason::Bounce, 1)])
            .await
            .unwrap();

        let found = adapter
            .find_suppressed(&["a@x.com".to_owned(), "b@x.com".to_owned()])
            .await
            .unwrap();
        assert_eq!(found, ["a@x.com".to_owned()].into_iter().collect());
        assert!(adapter.find_suppressed(&[]).await.unwrap().is_empty());
    }
}
