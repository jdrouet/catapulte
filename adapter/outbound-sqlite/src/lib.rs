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

use percent_encoding::percent_decode_str;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{ConnectOptions, SqlitePool};

// Mirrors how sqlx hands the name to SQLite: `vfs` or `immutable` wrap it in a
// `file:` URI, otherwise a `file:` name is itself a URI. Rebuilding a URL
// panics on names like `file::memory:`, hence the placeholder filename.
fn is_in_memory(opts: &SqliteConnectOptions) -> bool {
    let decode = |s: &str| percent_decode_str(s).decode_utf8_lossy().into_owned();
    let memory_pair = |k: &str, v: &str| matches!((k, v), ("mode", "memory") | ("vfs", "memdb"));
    let url = opts.clone().filename("db").to_url_lossy();
    let mut wrapped = false;
    for (k, v) in url.query_pairs() {
        if memory_pair(&k, &v) {
            return true;
        }
        wrapped |= k == "vfs" || k == "immutable";
    }
    let filename = opts.get_filename().to_string_lossy();
    if filename == ":memory:" {
        return true;
    }
    let Some(uri) = filename.strip_prefix("file:").filter(|_| !wrapped) else {
        return false;
    };
    let uri = uri.split('#').next().unwrap_or_default();
    let (path, query) = uri.split_once('?').unwrap_or((uri, ""));
    decode(path) == ":memory:"
        || query.split('&').any(|kv| {
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            memory_pair(&decode(k), &decode(v))
        })
}

/// An in-memory database vanishes with its last connection, so the single
/// connection must never be reaped, nor dropped by a cancelled checkout ping.
fn pool_options(base: SqlitePoolOptions, in_memory: bool) -> SqlitePoolOptions {
    let opts = base.max_connections(1);
    if in_memory {
        opts.idle_timeout(None)
            .max_lifetime(None)
            .test_before_acquire(false)
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
        let pool = pool_options(SqlitePoolOptions::new(), is_in_memory(&opts))
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
            "sqlite:file::memory:",
            "sqlite://file::memory:?cache=shared",
            "sqlite://foo.db?vfs=memdb",
            "sqlite://foo.db?mode=%6Demory",
            "sqlite://foo.db?mo%64e=memory",
            "sqlite:file:foo%3Fmode%3Dmemory",
            "sqlite://%3Amemory%3A",
            "sqlite:file:%253Amemory%253A",
            "sqlite:file::memory:#x",
            "sqlite://%3Amemory%3A?vfs=unix",
        ] {
            let opts = SqliteConnectOptions::from_str(url).unwrap();
            assert!(is_in_memory(&opts), "{url}");
        }
        for url in [
            "//:memory:",
            "sqlite://catapulte.db",
            "sqlite://memory.db",
            "sqlite://foo.db?mode=rwc",
            "sqlite://file:foo.db",
            "sqlite://foo.db?vfs=unix",
            "sqlite:file:memory.db",
            "sqlite:file::memory:?vfs=unix",
            "sqlite:file::memory:?immutable=true",
            "sqlite:FILE::memory:",
        ] {
            let opts = SqliteConnectOptions::from_str(url).unwrap();
            assert!(!is_in_memory(&opts), "{url}");
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

    #[tokio::test]
    async fn in_memory_database_survives_cancelled_checkouts() {
        let adapter = SqliteAdapter::connect("sqlite::memory:").await.unwrap();
        adapter.migrate().await.unwrap();

        for _ in 0..100 {
            drop(tokio::time::timeout(Duration::ZERO, adapter.pool().acquire()).await);
        }

        sqlx::query("SELECT count(*) FROM emails")
            .fetch_one(adapter.pool())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn connect_sets_pool_options_by_url_kind() {
        let mem = SqliteAdapter::connect("sqlite::memory:").await.unwrap();
        let o = mem.pool().options();
        assert_eq!(o.get_idle_timeout(), None);
        assert_eq!(o.get_max_lifetime(), None);
        assert!(!o.get_test_before_acquire());

        let path = std::env::temp_dir().join(format!("catapulte-{}.db", uuid::Uuid::now_v7()));
        let encoded = path
            .display()
            .to_string()
            .replace('%', "%25")
            .replace('?', "%3F");
        let file = SqliteAdapter::connect(&format!("sqlite://{encoded}"))
            .await
            .unwrap();
        let o = file.pool().options();
        assert!(o.get_idle_timeout().is_some());
        assert!(o.get_max_lifetime().is_some());
        assert!(o.get_test_before_acquire());
        drop(file);
        std::fs::remove_file(&path).unwrap();
    }

    #[tokio::test]
    async fn alternate_in_memory_urls_get_in_memory_options() {
        for url in [
            "sqlite://file::memory:",
            "sqlite://foo.db?vfs=memdb",
            "sqlite://foo.db?mode=%6Demory",
            "sqlite://%3Amemory%3A",
        ] {
            let adapter = SqliteAdapter::connect(url).await.unwrap();
            let o = adapter.pool().options();
            assert_eq!(o.get_idle_timeout(), None, "{url}");
            assert_eq!(o.get_max_lifetime(), None, "{url}");
            assert!(!o.get_test_before_acquire(), "{url}");
            adapter.migrate().await.unwrap();
        }
    }
}
