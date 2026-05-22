use thiserror::Error;

use crate::entity::body::{InterpolatedBody, ResolvedBody};

#[derive(Debug, Error)]
pub enum InterpolateError {
    #[error("interpolation engine error")]
    Engine {
        #[source]
        source: anyhow::Error,
    },
}

pub trait TemplateInterpolator {
    /// # Errors
    ///
    /// Returns an `InterpolateError` when the templating engine fails to process the body.
    fn interpolate(
        &self,
        body: ResolvedBody,
        variables: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<InterpolatedBody, InterpolateError>;
}
