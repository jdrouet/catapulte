use std::future::Future;

#[derive(Debug, thiserror::Error)]
pub enum HealthCheckError {
    #[error("dependency is unavailable")]
    Unavailable {
        #[source]
        source: anyhow::Error,
    },
}

pub trait HealthCheck: Send + Sync {
    /// # Errors
    ///
    /// Returns `HealthCheckError::Unavailable` when the dependency cannot be reached.
    fn check(&self) -> impl Future<Output = Result<(), HealthCheckError>> + Send;
}
