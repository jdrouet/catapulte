use std::time::Duration;

use thiserror::Error;

use crate::entity::email::EmailId;
use crate::entity::envelope::Envelope;

/// Opaque token returned by `dequeue` and passed back to `ack`/`nack`.
/// Each backend encodes whatever it needs (e.g. a UUID for SQL, a reply
/// subject for NATS).
#[derive(Debug, Clone)]
pub struct AckToken(pub Vec<u8>);

impl AckToken {
    #[must_use]
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }
}

/// Opaque W3C trace-context carrier. The domain holds it as plain string pairs
/// so the domain crate stays free of any OpenTelemetry dependency. Adapters
/// populate it on enqueue and extract it on dequeue.
#[derive(Debug, Clone, Default)]
pub struct TraceCarrier(Vec<(String, String)>);

impl TraceCarrier {
    #[must_use]
    pub fn new(pairs: Vec<(String, String)>) -> Self {
        Self(pairs)
    }

    #[must_use]
    pub fn into_pairs(self) -> Vec<(String, String)> {
        self.0
    }

    #[must_use]
    pub fn as_pairs(&self) -> &[(String, String)] {
        &self.0
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Value returned by [`EmailQueue::dequeue`].
pub struct DequeuedEmail {
    pub id: EmailId,
    pub envelope: Envelope,
    /// 1-based delivery attempt count.
    pub attempt: u32,
    /// Must be passed back to [`EmailQueue::ack`] or [`EmailQueue::nack`].
    pub token: AckToken,
    /// W3C trace-context headers captured at enqueue time. Empty when the
    /// backend does not support propagation or the row predates propagation.
    pub trace: TraceCarrier,
}

#[derive(Debug, Error)]
pub enum EmailQueueError {
    #[error("email queue error")]
    Storage {
        #[source]
        source: anyhow::Error,
    },
}

pub trait EmailQueue: Send + Sync + 'static {
    /// # Errors
    ///
    /// Returns `EmailQueueError::Storage` when the enqueue operation fails.
    fn enqueue(
        &self,
        id: EmailId,
        envelope: &Envelope,
    ) -> impl std::future::Future<Output = Result<(), EmailQueueError>> + Send;

    /// Dequeues the next available email.
    ///
    /// Blocks until an item is available. Returns a [`DequeuedEmail`] whose
    /// `token` must be passed to `ack` or `nack`.
    ///
    /// # Errors
    ///
    /// Returns `EmailQueueError::Storage` when the dequeue operation fails.
    fn dequeue(
        &self,
    ) -> impl std::future::Future<Output = Result<DequeuedEmail, EmailQueueError>> + Send;

    /// Acknowledges successful processing or permanent failure -- removes the
    /// item from the queue.
    ///
    /// # Errors
    ///
    /// Returns `EmailQueueError::Storage` when the ack operation fails.
    fn ack(
        &self,
        token: AckToken,
    ) -> impl std::future::Future<Output = Result<(), EmailQueueError>> + Send;

    /// Negatively acknowledges a transient failure. The item will become
    /// visible again after `delay`.
    ///
    /// # Errors
    ///
    /// Returns `EmailQueueError::Storage` when the nack operation fails.
    fn nack(
        &self,
        token: AckToken,
        delay: Duration,
    ) -> impl std::future::Future<Output = Result<(), EmailQueueError>> + Send;
}
