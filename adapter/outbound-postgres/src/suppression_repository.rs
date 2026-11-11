use anyhow::Context;
use catapulte_domain::entity::email::EmailId;
use catapulte_domain::entity::suppression::{Suppression, SuppressionReason};
use catapulte_domain::port::suppression_repository::{
    ListSuppressionsParams, SuppressionRepository, SuppressionRepositoryError,
};
use sqlx::{QueryBuilder, Row};

use crate::PostgresAdapter;

fn storage(source: anyhow::Error) -> SuppressionRepositoryError {
    SuppressionRepositoryError::Storage { source }
}

impl SuppressionRepository for PostgresAdapter {
    async fn list(
        &self,
        params: ListSuppressionsParams,
    ) -> Result<Vec<Suppression>, SuppressionRepositoryError> {
        let mut qb: QueryBuilder<sqlx::Postgres> = QueryBuilder::new(
            "SELECT address, reason, created_at_ms, source_email_id FROM suppressions WHERE 1=1",
        );
        if let Some(reason) = params.reason {
            qb.push(" AND reason = ");
            qb.push_bind(reason.as_str());
        }
        if let Some(address) = params.address {
            qb.push(" AND address ILIKE '%' || ");
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
                let source: Option<uuid::Uuid> = row
                    .try_get("source_email_id")
                    .context("reading source_email_id")?;
                Ok(Suppression {
                    address: row.try_get("address").context("reading address")?,
                    reason: reason.parse::<SuppressionReason>()?,
                    created_at_ms: row
                        .try_get("created_at_ms")
                        .context("reading created_at_ms")?,
                    source_email_id: source.map(EmailId::from),
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()
            .map_err(storage)
    }

    async fn add(&self, entries: &[Suppression]) -> Result<(), SuppressionRepositoryError> {
        for entry in entries {
            sqlx::query(
                "INSERT INTO suppressions (address, reason, created_at_ms, source_email_id) \
                 VALUES ($1, $2, $3, $4) ON CONFLICT (address) DO NOTHING",
            )
            .bind(&entry.address)
            .bind(entry.reason.as_str())
            .bind(entry.created_at_ms)
            .bind(entry.source_email_id.map(|id| id.as_uuid()))
            .execute(self.pool())
            .await
            .context("adding suppression")
            .map_err(storage)?;
        }
        Ok(())
    }

    async fn delete(&self, address: &str) -> Result<bool, SuppressionRepositoryError> {
        let result = sqlx::query("DELETE FROM suppressions WHERE address = $1")
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
        let found: Vec<String> =
            sqlx::query_scalar("SELECT address FROM suppressions WHERE address = ANY($1)")
                .bind(addresses)
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
