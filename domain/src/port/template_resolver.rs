use thiserror::Error;

use crate::entity::body::{BodySource, ResolvedBody};

#[derive(Debug, Error)]
pub enum ResolveError {
    #[error("template {name:?} not found")]
    NotFound { name: String },
    #[error("failed to fetch template from {url:?}")]
    Fetch {
        url: String,
        #[source]
        source: anyhow::Error,
    },
}

#[allow(async_fn_in_trait)]
pub trait TemplateResolver {
    /// # Errors
    ///
    /// Returns a `ResolveError` when the template cannot be found or fetched.
    async fn resolve(&self, body: BodySource) -> Result<ResolvedBody, ResolveError>;
}
