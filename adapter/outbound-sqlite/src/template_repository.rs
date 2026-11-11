use anyhow::Context;
use catapulte_domain::port::template_repository::{
    StoredTemplate, TemplateRepository, TemplateRepositoryError,
};
use sqlx::Row;

use crate::SqliteAdapter;

fn storage(source: anyhow::Error) -> TemplateRepositoryError {
    TemplateRepositoryError::Storage { source }
}

fn to_template(row: &sqlx::sqlite::SqliteRow) -> anyhow::Result<StoredTemplate> {
    let version: i64 = row.try_get("version").context("reading version")?;
    Ok(StoredTemplate {
        name: row.try_get("name").context("reading name")?,
        version: u32::try_from(version).context("version out of range")?,
        source: row.try_get("source").context("reading source")?,
        created_at_ms: row
            .try_get("created_at_ms")
            .context("reading created_at_ms")?,
    })
}

impl TemplateRepository for SqliteAdapter {
    async fn latest(&self, name: &str) -> Result<Option<StoredTemplate>, TemplateRepositoryError> {
        let row = sqlx::query(
            "SELECT name, version, source, created_at_ms FROM templates \
             WHERE name = ? ORDER BY version DESC LIMIT 1",
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
        let row = sqlx::query(
            "INSERT INTO templates (name, version, source, created_at_ms) \
             SELECT ?, COALESCE(MAX(version), 0) + 1, ?, ? FROM templates WHERE name = ? \
             RETURNING name, version, source, created_at_ms",
        )
        .bind(name)
        .bind(source)
        .bind(created_at_ms)
        .bind(name)
        .fetch_one(self.pool())
        .await
        .context("storing template")
        .map_err(storage)?;
        to_template(&row).map_err(storage)
    }

    async fn delete(&self, name: &str) -> Result<bool, TemplateRepositoryError> {
        let result = sqlx::query("DELETE FROM templates WHERE name = ?")
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

    use crate::SqliteAdapter;

    async fn fresh_adapter() -> SqliteAdapter {
        let adapter = SqliteAdapter::connect(":memory:").await.unwrap();
        adapter.migrate().await.unwrap();
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
}
