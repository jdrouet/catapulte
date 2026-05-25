use thiserror::Error;

use crate::port::attachment_store::AttachmentReader;

#[derive(Debug, Error)]
pub enum AttachmentFetchError {
    #[error("scheme not allowed: {scheme}")]
    SchemeNotAllowed { scheme: String },
    #[error("domain not allowed: {domain}")]
    DomainNotAllowed { domain: String },
    #[error("attachment exceeds maximum size")]
    TooLarge,
    #[error("attachment fetch failed")]
    Fetch {
        #[source]
        source: anyhow::Error,
    },
}

pub trait AttachmentFetcher: Send + Sync + 'static {
    /// # Errors
    ///
    /// Returns an `AttachmentFetchError` when policy rejects the URL or the
    /// HTTP fetch fails.
    fn fetch(
        &self,
        url: &url::Url,
    ) -> impl std::future::Future<Output = Result<AttachmentReader, AttachmentFetchError>> + Send;
}
