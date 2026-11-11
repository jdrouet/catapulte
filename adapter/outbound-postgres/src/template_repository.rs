use anyhow::Context;
use catapulte_domain::port::template_repository::{
    StoredTemplate, TemplateRepository, TemplateRepositoryError,
};
use sqlx::Row;

use crate::PostgresAdapter;

fn storage(source: anyhow::Error) -> TemplateRepositoryError {
    TemplateRepositoryError::Storage { source }
}

fn to_template(row: &sqlx::postgres::PgRow) -> anyhow::Result<StoredTemplate> {
    let version: i32 = row.try_get("version").context("reading version")?;
    Ok(StoredTemplate {
        name: row.try_get("name").context("reading name")?,
        version: u32::try_from(version).context("version out of range")?,
        source: row.try_get("source").context("reading source")?,
        created_at_ms: row
            .try_get("created_at_ms")
            .context("reading created_at_ms")?,
    })
}

const MAX_PUT_ATTEMPTS: u32 = 6;

fn is_unique_violation(e: &sqlx::Error) -> bool {
    e.as_database_error()
        .and_then(sqlx::error::DatabaseError::code)
        .is_some_and(|code| code == "23505")
}

impl PostgresAdapter {
    async fn insert_next_version(
        &self,
        name: &str,
        source: &str,
        created_at_ms: i64,
    ) -> Result<sqlx::postgres::PgRow, sqlx::Error> {
        let mut tx = self.pool().begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1))")
            .bind(name)
            .execute(&mut *tx)
            .await?;
        let row = sqlx::query(
            "INSERT INTO templates (name, version, source, created_at_ms) \
             SELECT $1::text, COALESCE(MAX(version), 0) + 1, $2::text, $3::bigint FROM templates WHERE name = $1::text \
             RETURNING name, version, source, created_at_ms",
        )
        .bind(name)
        .bind(source)
        .bind(created_at_ms)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(row)
    }
}

impl TemplateRepository for PostgresAdapter {
    async fn latest(&self, name: &str) -> Result<Option<StoredTemplate>, TemplateRepositoryError> {
        let row = sqlx::query(
            "SELECT name, version, source, created_at_ms FROM templates \
             WHERE name = $1 ORDER BY version DESC LIMIT 1",
        )
        .bind(name)
        .fetch_optional(self.pool())
        .await
        .context("reading template")
        .map_err(storage)?;
        row.as_ref().map(to_template).transpose().map_err(storage)
    }

    async fn list(&self) -> Result<Vec<StoredTemplate>, TemplateRepositoryError> {
        let rows = sqlx::query(
            "SELECT t.name, t.version, t.source, t.created_at_ms FROM templates t \
             WHERE t.version = (SELECT MAX(version) FROM templates WHERE name = t.name) \
             ORDER BY t.name",
        )
        .fetch_all(self.pool())
        .await
        .context("listing templates")
        .map_err(storage)?;
        rows.iter()
            .map(to_template)
            .collect::<anyhow::Result<Vec<_>>>()
            .map_err(storage)
    }

    async fn put(
        &self,
        name: &str,
        source: &str,
        created_at_ms: i64,
    ) -> Result<StoredTemplate, TemplateRepositoryError> {
        // The advisory lock serialises writers of one name; the retry covers any conflict that
        // still slips through, for example from a writer that bypasses this code.
        let mut attempt = 0;
        let row = loop {
            attempt += 1;
            match self.insert_next_version(name, source, created_at_ms).await {
                Ok(row) => break row,
                Err(e) if attempt < MAX_PUT_ATTEMPTS && is_unique_violation(&e) => {}
                Err(e) => return Err(storage(anyhow::Error::new(e).context("storing template"))),
            }
        };
        to_template(&row).map_err(storage)
    }

    async fn delete(&self, name: &str) -> Result<bool, TemplateRepositoryError> {
        let result = sqlx::query("DELETE FROM templates WHERE name = $1")
            .bind(name)
            .execute(self.pool())
            .await
            .context("deleting template")
            .map_err(storage)?;
        Ok(result.rows_affected() > 0)
    }
}

#[cfg(test)]
mod tests {
    use catapulte_domain::port::template_repository::TemplateRepository;

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

    #[tokio::test]
    async fn put_creates_versions_and_latest_wins() {
        let adapter = fresh_adapter().await;

        let first = adapter.put("welcome", "<mjml/>", 1).await.unwrap();
        let second = adapter.put("welcome", "<mjml>2</mjml>", 2).await.unwrap();
        adapter.put("alpha", "a", 3).await.unwrap();

        assert_eq!((first.version, second.version), (1, 2));
        let latest = adapter.latest("welcome").await.unwrap().unwrap();
        assert_eq!(latest.source, "<mjml>2</mjml>");
        assert!(adapter.latest("missing").await.unwrap().is_none());
        let listed = adapter.list().await.unwrap();
        let names: Vec<_> = listed
            .iter()
            .map(|t| (t.name.as_str(), t.version))
            .collect();
        assert_eq!(names, [("alpha", 1), ("welcome", 2)]);
    }

    #[tokio::test]
    async fn delete_removes_every_version_and_reports_whether_it_existed() {
        let adapter = fresh_adapter().await;
        adapter.put("welcome", "a", 1).await.unwrap();
        adapter.put("welcome", "b", 2).await.unwrap();

        assert!(adapter.delete("welcome").await.unwrap());
        assert!(!adapter.delete("welcome").await.unwrap());
        assert!(adapter.latest("welcome").await.unwrap().is_none());
        assert_eq!(adapter.put("welcome", "c", 3).await.unwrap().version, 1);
    }

    #[tokio::test]
    async fn concurrent_puts_of_one_name_get_consecutive_versions() {
        let adapter = fresh_adapter().await;

        let tasks: Vec<_> = (0..8)
            .map(|i| {
                let adapter = adapter.clone();
                tokio::spawn(async move {
                    adapter
                        .put("fresh", &format!("source {i}"), i)
                        .await
                        .unwrap()
                        .version
                })
            })
            .collect();
        let mut versions = Vec::new();
        for task in tasks {
            versions.push(task.await.unwrap());
        }
        versions.sort_unstable();

        assert_eq!(versions, (1..=8).collect::<Vec<u32>>());
        assert_eq!(adapter.latest("fresh").await.unwrap().unwrap().version, 8);
    }
}
