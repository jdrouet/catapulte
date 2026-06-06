use catapulte_domain::port::health::{HealthCheck, HealthCheckError};

use crate::queue::QueueAdapter;
use crate::storage::StorageAdapter;

#[derive(Clone)]
pub(crate) struct ReadinessProbe {
    storage: StorageAdapter,
    queue: QueueAdapter,
}

impl ReadinessProbe {
    pub(crate) fn new(storage: StorageAdapter, queue: QueueAdapter) -> Self {
        Self { storage, queue }
    }
}

impl HealthCheck for ReadinessProbe {
    async fn check(&self) -> Result<(), HealthCheckError> {
        self.storage.check().await?;
        self.queue.check().await?;
        Ok(())
    }
}
