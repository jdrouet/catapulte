use catapulte_domain::port::health::{HealthCheck, HealthCheckError};

#[allow(unknown_lints, clippy::unused_async_trait_impl)] // lint is newer than the CI toolchain's clippy
impl HealthCheck for crate::NatsAdapter {
    async fn check(&self) -> Result<(), HealthCheckError> {
        match self.client().connection_state() {
            async_nats::connection::State::Connected => Ok(()),
            other => Err(HealthCheckError::Unavailable {
                source: anyhow::anyhow!("NATS connection not ready: {other:?}"),
            }),
        }
    }
}
