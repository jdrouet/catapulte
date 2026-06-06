use catapulte_domain::entity::attachment::BlobRef;
use catapulte_domain::port::attachment_store::{
    AttachmentReader, AttachmentStore, AttachmentStoreError, PutResult,
};
use catapulte_outbound_attachment_fs::store::{FsAttachmentStore, FsAttachmentStoreConfig};
use catapulte_outbound_attachment_redis::store::{
    RedisAttachmentStore, RedisAttachmentStoreConfig,
};
use catapulte_outbound_attachment_s3::store::{S3AttachmentStore, S3AttachmentStoreConfig};

#[derive(Clone)]
pub enum AttachmentStoreAdapter {
    Fs(FsAttachmentStore),
    S3(S3AttachmentStore),
    Redis(RedisAttachmentStore),
}

impl AttachmentStore for AttachmentStoreAdapter {
    #[tracing::instrument(skip_all, name = "attachment.put", fields(backend = self.backend_name()))]
    async fn put(&self, reader: AttachmentReader) -> Result<PutResult, AttachmentStoreError> {
        match self {
            Self::Fs(s) => s.put(reader).await,
            Self::S3(s) => s.put(reader).await,
            Self::Redis(s) => s.put(reader).await,
        }
    }

    #[tracing::instrument(skip_all, name = "attachment.get", fields(backend = self.backend_name()))]
    async fn get(&self, blob: &BlobRef) -> Result<AttachmentReader, AttachmentStoreError> {
        match self {
            Self::Fs(s) => s.get(blob).await,
            Self::S3(s) => s.get(blob).await,
            Self::Redis(s) => s.get(blob).await,
        }
    }

    async fn delete(&self, blob: &BlobRef) -> Result<(), AttachmentStoreError> {
        match self {
            Self::Fs(s) => s.delete(blob).await,
            Self::S3(s) => s.delete(blob).await,
            Self::Redis(s) => s.delete(blob).await,
        }
    }
}

pub enum AttachmentStoreBackendConfig {
    Fs(FsAttachmentStoreConfig),
    S3(S3AttachmentStoreConfig),
    Redis(RedisAttachmentStoreConfig),
}

impl AttachmentStoreBackendConfig {
    /// # Errors
    ///
    /// Returns an error when the backend name is unknown or a required env var is missing.
    pub fn from_env() -> anyhow::Result<Self> {
        let backend =
            std::env::var("CATAPULTE_ATTACHMENT_BACKEND").unwrap_or_else(|_| "fs".to_owned());
        match backend.as_str() {
            "fs" => Ok(Self::Fs(FsAttachmentStoreConfig::from_env(
                "CATAPULTE_ATTACHMENT_FS",
            )?)),
            "s3" => Ok(Self::S3(S3AttachmentStoreConfig::from_env(
                "CATAPULTE_ATTACHMENT_S3",
            )?)),
            "redis" => Ok(Self::Redis(RedisAttachmentStoreConfig::from_env(
                "CATAPULTE_ATTACHMENT_REDIS",
            )?)),
            other => anyhow::bail!(
                "unknown attachment backend {other:?} in env var CATAPULTE_ATTACHMENT_BACKEND"
            ),
        }
    }

    /// # Errors
    ///
    /// Returns an error when the store fails to initialise (e.g. cannot create directory).
    pub async fn build(self) -> anyhow::Result<AttachmentStoreAdapter> {
        match self {
            Self::Fs(cfg) => Ok(AttachmentStoreAdapter::Fs(cfg.build().await?)),
            Self::S3(cfg) => Ok(AttachmentStoreAdapter::S3(cfg.build().await?)),
            Self::Redis(cfg) => Ok(AttachmentStoreAdapter::Redis(cfg.build().await?)),
        }
    }
}

impl AttachmentStoreAdapter {
    /// Returns a short string identifying the active backend (e.g. for metrics or logs).
    #[must_use]
    pub fn backend_name(&self) -> &'static str {
        match self {
            Self::Fs(_) => "fs",
            Self::S3(_) => "s3",
            Self::Redis(_) => "redis",
        }
    }

    /// Returns object keys that are older than `age` across the active backend.
    ///
    /// # Errors
    ///
    /// Returns an error when the underlying list operation fails.
    pub async fn list_keys_older_than(
        &self,
        age: std::time::Duration,
    ) -> anyhow::Result<Vec<String>> {
        match self {
            Self::Fs(s) => s.list_keys_older_than(age).await,
            Self::S3(s) => s.list_keys_older_than(age).await,
            Self::Redis(s) => s.list_keys_older_than(age).await,
        }
    }
}
