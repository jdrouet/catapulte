use anyhow::Context;
use catapulte_domain::port::health::{HealthCheck, HealthCheckError};

impl HealthCheck for crate::PostgresAdapter {
    async fn check(&self) -> Result<(), HealthCheckError> {
        sqlx::query("SELECT 1")
            .execute(self.pool())
            .await
            .context("postgres readiness probe failed")
            .map_err(|source| HealthCheckError::Unavailable { source })?;
        Ok(())
    }
}
