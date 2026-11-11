use thiserror::Error;

use crate::entity::body::{InterpolatedBody, ResolvedBody};

#[derive(Debug, Error)]
pub enum InterpolateError {
    /// The template used up its render budget (for example an enormous nested loop). Rendering
    /// is deterministic, so retrying cannot succeed.
    #[error("template rendering exceeded its budget (too many loop iterations or operations)")]
    BudgetExceeded,
    #[error("interpolation engine error")]
    Engine {
        #[source]
        source: anyhow::Error,
    },
    /// The render sandbox itself failed (spawn, crash, abort); the template is not at fault, so
    /// retrying can succeed.
    #[error("template rendering infrastructure failure")]
    Infrastructure {
        #[source]
        source: anyhow::Error,
    },
}

pub trait TemplateInterpolator: Send + Sync + 'static {
    /// # Errors
    ///
    /// Returns an `InterpolateError` when the templating engine fails to process the body.
    fn interpolate(
        &self,
        body: ResolvedBody,
        variables: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<InterpolatedBody, InterpolateError>;
}
