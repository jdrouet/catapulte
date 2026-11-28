use catapulte_domain::port::health::{HealthCheck, HealthCheckError};

impl HealthCheck for crate::NatsAdapter {
    fn check(&self) -> impl Future<Output = Result<(), HealthCheckError>> {
        std::future::ready(match self.client().connection_state() {
            async_nats::connection::State::Connected => Ok(()),
            other => Err(HealthCheckError::Unavailable {
                source: anyhow::anyhow!("NATS connection not ready: {other:?}"),
            }),
        })
    }
}
