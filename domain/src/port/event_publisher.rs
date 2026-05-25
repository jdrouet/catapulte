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

pub trait EventPublisher: Send + Sync + 'static {
    /// # Errors
    ///
    /// Returns an `EventPublisherError` when the event cannot be published.
    fn publish(
        &self,
        event: &LifecycleEvent,
    ) -> impl std::future::Future<Output = Result<(), EventPublisherError>> + Send;
}
