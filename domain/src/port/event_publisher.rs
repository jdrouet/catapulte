use thiserror::Error;

use crate::entity::lifecycle_event::LifecycleEvent;

#[derive(Debug, Error)]
pub enum EventPublisherError {
    #[error("event publish failed")]
    Publish {
        #[source]
        source: anyhow::Error,
    },
}

#[allow(async_fn_in_trait)]
pub trait EventPublisher {
    /// # Errors
    ///
    /// Returns an `EventPublisherError` when the event cannot be published.
    async fn publish(&self, event: &LifecycleEvent) -> Result<(), EventPublisherError>;
}
