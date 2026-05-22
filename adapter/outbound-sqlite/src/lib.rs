pub mod dto;
pub mod email_repository;
pub mod event_publisher;

use std::str::FromStr;

use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

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
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
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
