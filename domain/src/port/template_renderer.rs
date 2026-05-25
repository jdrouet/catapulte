use thiserror::Error;

use crate::entity::body::{InterpolatedBody, RenderedBody};

#[derive(Debug, Error)]
pub enum RenderError {
    #[error("mjml render error")]
    Mjml {
        #[source]
        source: anyhow::Error,
    },
}

pub trait TemplateRenderer: Send + Sync + 'static {
    /// # Errors
    ///
    /// Returns a `RenderError` when the mjml renderer fails to compile the body.
    fn render(&self, body: InterpolatedBody) -> Result<RenderedBody, RenderError>;
}
