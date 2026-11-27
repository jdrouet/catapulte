pub mod dto;
pub mod email_lookup;
pub mod email_queue;
pub mod email_repository;
pub mod event_publisher;
pub mod event_repository;
mod health;
pub mod sender_usage;
pub mod suppression_repository;
pub mod template_repository;

use std::str::FromStr;

use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

fn is_in_memory(url: &str) -> bool {
    let rest = url.strip_prefix("sqlite:").unwrap_or(url);
    let rest = rest.strip_prefix("//").unwrap_or(rest);
    let (db, query) = rest.split_once('?').unwrap_or((rest, ""));
    db == ":memory:" || query.split('&').any(|kv| kv == "mode=memory")
}

/// An in-memory database vanishes with its last connection, so the single
/// connection must never be reaped.
fn pool_options(base: SqlitePoolOptions, in_memory: bool) -> SqlitePoolOptions {
    let opts = base.max_connections(1);
    if in_memory {
        opts.idle_timeout(None).max_lifetime(None)
    } else {
        opts
    }
}

#[derive(Clone, Debug)]
pub struct SqliteAdapter {
    pool: SqlitePool,
}

impl SqliteAdapter {
    /// # Errors
    ///
    /// Returns an error if the database cannot be opened.
    pub async fn connect(url: &str) -> anyhow::Result<Self> {
        use anyhow::Context;
        let opts = SqliteConnectOptions::from_str(url)
            .context("invalid sqlite url")?
            .create_if_missing(true);
        let pool = pool_options(SqlitePoolOptions::new(), is_in_memory(url))
            .connect_with(opts)
            .await
            .context("failed to open sqlite pool")?;
        Ok(Self { pool })
    }

    /// # Errors
    ///
    /// Returns an error if the migrations fail to run.
    pub async fn migrate(&self) -> anyhow::Result<()> {
        use anyhow::Context;
        sqlx::migrate!("./migrations")
            .run(&self.pool)
            .await
            .context("failed to run sqlite migrations")?;
        Ok(())
    }

    pub(crate) fn pool(&self) -> &SqlitePool {
        &self.pool
    }
}

pub struct SqliteConfig {
    pub url: String,
}

impl SqliteConfig {
    /// # Errors
    ///
    /// Returns an error if the required environment variable is not set.
    pub fn from_env(prefix: &str) -> anyhow::Result<Self> {
        use anyhow::Context;
        let url_key = format!("{prefix}_URL");
        let url = std::env::var(&url_key).with_context(|| format!("missing env var {url_key}"))?;
        Ok(Self { url })
    }

    /// # Errors
    ///
    /// Returns an error if the database cannot be opened.
    pub async fn build(self) -> anyhow::Result<SqliteAdapter> {
        SqliteAdapter::connect(&self.url).await
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn detects_in_memory_urls() {
        for url in [
            ":memory:",
            "sqlite::memory:",
            "sqlite://:memory:",
            "sqlite://foo.db?mode=memory",
            "sqlite:foo?mode=memory&cache=shared",
        ] {
            assert!(is_in_memory(url), "{url}");
        }
        for url in [
            "sqlite://catapulte.db",
            "sqlite://memory.db",
            "sqlite://foo.db?mode=rwc",
        ] {
            assert!(!is_in_memory(url), "{url}");
        }
    }

    #[tokio::test]
    async fn in_memory_database_survives_pool_timeouts() {
        let short = Some(Duration::from_millis(50));
        let base = SqlitePoolOptions::new()
            .idle_timeout(short)
            .max_lifetime(short);
        let opts = SqliteConnectOptions::from_str("sqlite::memory:").unwrap();
        let pool = pool_options(base, true).connect_with(opts).await.unwrap();
        let adapter = SqliteAdapter { pool };
        adapter.migrate().await.unwrap();

        tokio::time::sleep(Duration::from_millis(300)).await;

        sqlx::query("SELECT count(*) FROM emails")
            .fetch_one(adapter.pool())
            .await
            .unwrap();
    }
}
