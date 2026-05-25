use catapulte_domain::entity::attachment::BlobRef;
use catapulte_domain::port::attachment_store::{
    AttachmentReader, AttachmentStore, AttachmentStoreError, PutResult,
};
use catapulte_outbound_attachment_fs::store::{FsAttachmentStore, FsAttachmentStoreConfig};

#[derive(Clone)]
pub enum AttachmentStoreAdapter {
    Fs(FsAttachmentStore),
}

impl AttachmentStore for AttachmentStoreAdapter {
    async fn put(&self, reader: AttachmentReader) -> Result<PutResult, AttachmentStoreError> {
        match self {
            Self::Fs(s) => s.put(reader).await,
        }
    }

    async fn get(&self, blob: &BlobRef) -> Result<AttachmentReader, AttachmentStoreError> {
        match self {
            Self::Fs(s) => s.get(blob).await,
        }
    }

    async fn delete(&self, blob: &BlobRef) -> Result<(), AttachmentStoreError> {
        match self {
            Self::Fs(s) => s.delete(blob).await,
        }
    }
}

pub enum AttachmentStoreBackendConfig {
    Fs(FsAttachmentStoreConfig),
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
            other => anyhow::bail!("unknown attachment backend {other:?}"),
        }
    }

    /// # Errors
    ///
    /// Returns an error when the store fails to initialise (e.g. cannot create directory).
    pub async fn build(self) -> anyhow::Result<AttachmentStoreAdapter> {
        match self {
            Self::Fs(cfg) => Ok(AttachmentStoreAdapter::Fs(cfg.build().await?)),
        }
    }
}
