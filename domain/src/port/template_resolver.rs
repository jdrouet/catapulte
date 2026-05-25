use thiserror::Error;

use crate::entity::body::{BodySource, ResolvedBody};

#[derive(Debug, Error)]
pub enum ResolveError {
    #[error("template {name:?} not found")]
    NotFound { name: String },
    #[error("domain not allowed for remote template: {url:?}")]
    DomainNotAllowed { url: String },
    #[error("failed to fetch template from {url:?}")]
    Fetch {
        url: String,
        #[source]
        source: anyhow::Error,
    },
}

pub trait TemplateResolver: Send + Sync + 'static {
    /// # Errors
    ///
    /// Returns a `ResolveError` when the template cannot be found or fetched.
    fn resolve(
        &self,
        body: BodySource,
    ) -> impl std::future::Future<Output = Result<ResolvedBody, ResolveError>> + Send;
}
