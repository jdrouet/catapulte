use std::pin::Pin;

use thiserror::Error;
use tokio::io::AsyncRead;

use crate::entity::attachment::BlobRef;

#[derive(Debug, Error)]
pub enum AttachmentStoreError {
    #[error("attachment store I/O failed")]
    Io {
        #[source]
        source: anyhow::Error,
    },
    #[error("attachment not found")]
    NotFound,
}

pub struct PutResult {
    pub blob: BlobRef,
    pub size_bytes: u64,
}

pub type AttachmentReader = Pin<Box<dyn AsyncRead + Send>>;

pub trait AttachmentStore: Send + Sync + 'static {
    /// # Errors
    ///
    /// Returns `AttachmentStoreError::Io` when the underlying store fails.
    fn put(
        &self,
        reader: AttachmentReader,
    ) -> impl std::future::Future<Output = Result<PutResult, AttachmentStoreError>> + Send;

    /// # Errors
    ///
    /// Returns `AttachmentStoreError::NotFound` when the blob is missing.
    /// Returns `AttachmentStoreError::Io` when the underlying store fails.
    fn get(
        &self,
        blob: &BlobRef,
    ) -> impl std::future::Future<Output = Result<AttachmentReader, AttachmentStoreError>> + Send;

    /// # Errors
    ///
    /// Returns `AttachmentStoreError::Io` when the underlying store fails.
    /// A missing blob is NOT an error — delete is idempotent.
    fn delete(
        &self,
        blob: &BlobRef,
    ) -> impl std::future::Future<Output = Result<(), AttachmentStoreError>> + Send;
}
