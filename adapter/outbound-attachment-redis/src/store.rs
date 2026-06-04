use std::io::Cursor;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Context;
use catapulte_domain::entity::attachment::BlobRef;
use catapulte_domain::port::attachment_store::{
    AttachmentReader, AttachmentStore, AttachmentStoreError, PutResult,
};
use redis::AsyncCommands;
use redis::aio::ConnectionManager;
use tokio::io::AsyncReadExt as _;

#[derive(Debug)]
pub struct RedisAttachmentStoreConfig {
    pub url: String,
    pub prefix: String,
}

impl RedisAttachmentStoreConfig {
    /// # Errors
    ///
    /// Returns an error when the required `<prefix>_URL` env var is missing.
    pub fn from_env(prefix: &str) -> anyhow::Result<Self> {
        let key_url = format!("{prefix}_URL");
        let url = std::env::var(&key_url).with_context(|| format!("missing env var {key_url}"))?;
        let store_prefix = std::env::var(format!("{prefix}_PREFIX")).unwrap_or_default();
        Ok(Self {
            url,
            prefix: store_prefix,
        })
    }

    /// # Errors
    ///
    /// Returns an error when the URL is invalid or the connection cannot be established.
    pub async fn build(self) -> anyhow::Result<RedisAttachmentStore> {
        RedisAttachmentStore::connect(self).await
    }
}

#[derive(Clone)]
pub struct RedisAttachmentStore {
    conn: ConnectionManager,
    prefix: Arc<str>,
    index_key: Arc<str>,
}

impl RedisAttachmentStore {
    /// # Errors
    ///
    /// Returns an error when the URL is invalid or the connection cannot be established.
    pub async fn connect(cfg: RedisAttachmentStoreConfig) -> anyhow::Result<Self> {
        let client = redis::Client::open(cfg.url).context("invalid redis url")?;
        let conn = ConnectionManager::new(client)
            .await
            .context("failed to connect to redis")?;
        let index_key = format!("{}catapulte:attachment-index", cfg.prefix);
        Ok(Self {
            conn,
            prefix: Arc::from(cfg.prefix.as_str()),
            index_key: Arc::from(index_key.as_str()),
        })
    }

    fn generate_key(&self) -> String {
        format!("{}{}", self.prefix, uuid::Uuid::now_v7().simple())
    }

    /// Returns blob keys whose creation timestamp is older than `age`.
    ///
    /// Creation times are tracked in a Redis sorted set scored by epoch
    /// seconds; this returns the members with a score at or below `now - age`.
    ///
    /// # Errors
    ///
    /// Returns an error when the Redis query fails.
    pub async fn list_keys_older_than(&self, age: Duration) -> anyhow::Result<Vec<String>> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_secs();
        let cutoff = now.saturating_sub(age.as_secs());
        let cutoff = i64::try_from(cutoff).unwrap_or(i64::MAX);

        let mut conn = self.conn.clone();
        let keys: Vec<String> = conn
            .zrangebyscore(self.index_key.as_ref(), 0i64, cutoff)
            .await
            .context("redis ZRANGEBYSCORE failed")?;
        Ok(keys)
    }
}

fn redis_err_to_io(e: redis::RedisError, context: &'static str) -> AttachmentStoreError {
    AttachmentStoreError::Io {
        source: anyhow::Error::new(e).context(context),
    }
}

impl AttachmentStore for RedisAttachmentStore {
    async fn put(&self, mut reader: AttachmentReader) -> Result<PutResult, AttachmentStoreError> {
        let mut buf = Vec::new();
        reader
            .read_to_end(&mut buf)
            .await
            .map_err(|e| AttachmentStoreError::Io {
                source: anyhow::Error::new(e).context("failed to read attachment body"),
            })?;

        let key = self.generate_key();
        let size_bytes = u64::try_from(buf.len()).unwrap_or(u64::MAX);
        let now = i64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or(Duration::ZERO)
                .as_secs(),
        )
        .unwrap_or(i64::MAX);

        let mut conn = self.conn.clone();
        // Store the blob and index its creation time atomically so the GC sweep
        // never sees a blob without a timestamp or vice versa.
        redis::pipe()
            .atomic()
            .set(&key, buf)
            .ignore()
            .zadd(self.index_key.as_ref(), &key, now)
            .ignore()
            .query_async::<()>(&mut conn)
            .await
            .map_err(|e| redis_err_to_io(e, "failed to store attachment in redis"))?;

        Ok(PutResult {
            blob: BlobRef {
                backend: "redis".into(),
                key,
            },
            size_bytes,
        })
    }

    async fn get(&self, blob: &BlobRef) -> Result<AttachmentReader, AttachmentStoreError> {
        let mut conn = self.conn.clone();
        let value: Option<Vec<u8>> = conn
            .get(&blob.key)
            .await
            .map_err(|e| redis_err_to_io(e, "failed to get attachment from redis"))?;

        match value {
            Some(bytes) => Ok(Box::pin(Cursor::new(bytes))),
            None => Err(AttachmentStoreError::NotFound),
        }
    }

    async fn delete(&self, blob: &BlobRef) -> Result<(), AttachmentStoreError> {
        let mut conn = self.conn.clone();
        // Remove the blob and its index entry. DEL and ZREM on missing members
        // are no-ops, so delete is idempotent.
        redis::pipe()
            .atomic()
            .del(&blob.key)
            .ignore()
            .zrem(self.index_key.as_ref(), &blob.key)
            .ignore()
            .query_async::<()>(&mut conn)
            .await
            .map_err(|e| redis_err_to_io(e, "failed to delete attachment from redis"))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::time::Duration;

    use catapulte_domain::entity::attachment::BlobRef;
    use catapulte_domain::port::attachment_store::{
        AttachmentReader, AttachmentStore, AttachmentStoreError,
    };
    use tokio::io::AsyncReadExt;

    use super::{RedisAttachmentStore, RedisAttachmentStoreConfig};

    fn reader_from(data: &[u8]) -> AttachmentReader {
        Box::pin(Cursor::new(data.to_vec()))
    }

    #[test]
    fn from_env_missing_url_returns_error() {
        let err = RedisAttachmentStoreConfig::from_env("CATAPULTE_REDIS_TEST_ABSENT_URL_XZQR9");
        assert!(err.is_err(), "expected error for missing url");
        assert!(
            err.unwrap_err().to_string().contains("URL"),
            "error should mention URL"
        );
    }

    async fn fresh_store() -> (
        RedisAttachmentStore,
        testcontainers::ContainerAsync<testcontainers::GenericImage>,
    ) {
        use testcontainers::GenericImage;
        use testcontainers::core::WaitFor;
        use testcontainers::runners::AsyncRunner;

        let container = GenericImage::new("redis", "7-alpine")
            .with_wait_for(WaitFor::message_on_stdout("Ready to accept connections"))
            .start()
            .await
            .expect("failed to start Redis container; ensure Docker is running");

        let port = container.get_host_port_ipv4(6379).await.unwrap();
        let store = RedisAttachmentStoreConfig {
            url: format!("redis://127.0.0.1:{port}"),
            prefix: String::new(),
        }
        .build()
        .await
        .expect("failed to build Redis attachment store");

        (store, container)
    }

    #[serial_test::serial]
    #[tokio::test]
    async fn put_then_get_roundtrips_bytes() {
        let (store, _container) = fresh_store().await;
        let payload = b"hello, redis!";
        let result = store.put(reader_from(payload)).await.expect("put");

        let mut reader = store.get(&result.blob).await.expect("get");
        let mut buf = Vec::new();
        reader.read_to_end(&mut buf).await.expect("read");
        assert_eq!(buf, payload);
    }

    #[serial_test::serial]
    #[tokio::test]
    async fn put_returns_size_matching_input() {
        let (store, _container) = fresh_store().await;
        let payload = b"some data here";
        let result = store.put(reader_from(payload)).await.expect("put");
        assert_eq!(
            result.size_bytes,
            u64::try_from(payload.len()).unwrap(),
            "size_bytes should match input length"
        );
    }

    #[serial_test::serial]
    #[tokio::test]
    async fn delete_nonexistent_is_idempotent() {
        let (store, _container) = fresh_store().await;
        let blob = BlobRef {
            backend: "redis".into(),
            key: "nonexistent".into(),
        };
        store
            .delete(&blob)
            .await
            .expect("delete of missing key should be Ok");
    }

    #[serial_test::serial]
    #[tokio::test]
    async fn put_delete_get_returns_not_found() {
        let (store, _container) = fresh_store().await;
        let payload = b"to be deleted";
        let result = store.put(reader_from(payload)).await.expect("put");
        store.delete(&result.blob).await.expect("delete");

        let get_result = store.get(&result.blob).await;
        assert!(
            matches!(get_result, Err(AttachmentStoreError::NotFound)),
            "expected NotFound after delete"
        );
    }

    #[serial_test::serial]
    #[tokio::test]
    async fn get_missing_key_returns_not_found() {
        let (store, _container) = fresh_store().await;
        let blob = BlobRef {
            backend: "redis".into(),
            key: "does-not-exist".into(),
        };
        let result = store.get(&blob).await;
        assert!(
            matches!(result, Err(AttachmentStoreError::NotFound)),
            "expected NotFound for missing key"
        );
    }

    #[serial_test::serial]
    #[tokio::test]
    async fn list_keys_older_than_max_empty_zero_all() {
        let (store, _container) = fresh_store().await;

        let r1 = store.put(reader_from(b"blob one")).await.expect("put 1");
        let r2 = store.put(reader_from(b"blob two")).await.expect("put 2");

        // With Duration::MAX nothing qualifies.
        let none = store
            .list_keys_older_than(Duration::MAX)
            .await
            .expect("list_keys_older_than MAX");
        assert!(
            none.is_empty(),
            "expected no keys older than MAX, got: {none:?}"
        );

        // With Duration::ZERO everything qualifies.
        let mut all = store
            .list_keys_older_than(Duration::ZERO)
            .await
            .expect("list_keys_older_than ZERO");
        all.sort();
        let mut expected = vec![r1.blob.key.clone(), r2.blob.key.clone()];
        expected.sort();
        assert_eq!(all, expected, "expected both keys with age ZERO");
    }
}
