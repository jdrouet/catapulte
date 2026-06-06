use catapulte_domain::port::health::{HealthCheck, HealthCheckError};

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
