pub mod dto;
pub mod email_queue;
pub mod email_repository;
pub mod event_publisher;
pub mod event_repository;
mod health;
pub mod sender_usage;

use anyhow::Context;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

#[derive(Clone, Debug)]
pub struct PostgresAdapter {
    pool: PgPool,
}

impl PostgresAdapter {
    /// # Errors
    ///
    /// Returns an error if the database cannot be opened.
    pub async fn connect(
        url: &str,
        max_connections: u32,
        acquire_timeout: std::time::Duration,
    ) -> anyhow::Result<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(max_connections)
            .acquire_timeout(acquire_timeout)
            .connect(url)
            .await
            .context("failed to open postgres pool")?;
        Ok(Self { pool })
    }

    /// # Errors
    ///
    /// Returns an error if the migrations fail to run.
    pub async fn migrate(&self) -> anyhow::Result<()> {
        sqlx::migrate!("./migrations")
            .run(&self.pool)
            .await
            .context("failed to run postgres migrations")?;
        Ok(())
    }

    pub(crate) fn pool(&self) -> &PgPool {
        &self.pool
    }
}

pub struct PostgresConfig {
    pub url: String,
    pub max_connections: u32,
    pub acquire_timeout: std::time::Duration,
}

impl PostgresConfig {
    /// # Errors
    ///
    /// Returns an error if the required URL environment variable is not set, or
    /// if a pool tuning variable is present but cannot be parsed.
    pub fn from_env(prefix: &str) -> anyhow::Result<Self> {
        let url_key = format!("{prefix}_URL");
        let url = std::env::var(&url_key).with_context(|| format!("missing env var {url_key}"))?;
        let max_connections_key = format!("{prefix}_MAX_CONNECTIONS");
        let max_connections: u32 = match std::env::var(&max_connections_key) {
            Err(std::env::VarError::NotPresent) => 10,
            Err(e) => {
                return Err(anyhow::Error::new(e).context(format!("reading {max_connections_key}")));
            }
            Ok(v) => v
                .parse()
                .with_context(|| format!("invalid {max_connections_key}: {v:?}"))?,
        };
        let acquire_timeout_key = format!("{prefix}_ACQUIRE_TIMEOUT_SECS");
        let acquire_timeout_secs: u64 = match std::env::var(&acquire_timeout_key) {
            Err(std::env::VarError::NotPresent) => 30,
            Err(e) => {
                return Err(anyhow::Error::new(e).context(format!("reading {acquire_timeout_key}")));
            }
            Ok(v) => v
                .parse()
                .with_context(|| format!("invalid {acquire_timeout_key}: {v:?}"))?,
        };
        let acquire_timeout = std::time::Duration::from_secs(acquire_timeout_secs);
        Ok(Self {
            url,
            max_connections,
            acquire_timeout,
        })
    }

    /// # Errors
    ///
    /// Returns an error if the database cannot be opened.
    pub async fn build(self) -> anyhow::Result<PostgresAdapter> {
        PostgresAdapter::connect(&self.url, self.max_connections, self.acquire_timeout).await
    }
}
