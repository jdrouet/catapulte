use crate::port::template_interpolator::InterpolateError;

/// Checks a template at upload time without the strictness of send time.
pub trait TemplateValidator: Send + Sync + 'static {
    /// Renders `source` with `variables` and returns the result. Missing variables and missing
    /// nested attributes render as empty; operations that need a real value (arithmetic, some
    /// filters) still fail, so callers pass representative `variables`.
    ///
    /// # Errors
    ///
    /// Returns an `InterpolateError` when the source does not parse or cannot render.
    fn validate(
        &self,
        source: &str,
        variables: &serde_json::Map<String, serde_json::Value>,
    ) -> impl std::future::Future<Output = Result<String, InterpolateError>> + Send;
}
