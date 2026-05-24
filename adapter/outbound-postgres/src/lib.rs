pub mod dto;
pub mod email_queue;
pub mod email_repository;
pub mod event_publisher;

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
    pub async fn connect(url: &str) -> anyhow::Result<Self> {
        let pool = PgPoolOptions::new()
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
}

impl PostgresConfig {
    /// # Errors
    ///
    /// Returns an error if the required environment variable is not set.
    pub fn from_env(prefix: &str) -> anyhow::Result<Self> {
        let url_key = format!("{prefix}_URL");
        let url = std::env::var(&url_key).with_context(|| format!("missing env var {url_key}"))?;
        Ok(Self { url })
    }

    /// # Errors
    ///
    /// Returns an error if the database cannot be opened.
    pub async fn build(self) -> anyhow::Result<PostgresAdapter> {
        PostgresAdapter::connect(&self.url).await
    }
}
