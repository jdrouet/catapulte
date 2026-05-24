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

#[derive(Debug, Error)]
pub enum EmailQueueError {
    #[error("email queue error")]
    Storage {
        #[source]
        source: anyhow::Error,
    },
}

pub trait EmailQueue {
    fn enqueue(
        &self,
        id: EmailId,
        envelope: &Envelope,
    ) -> impl std::future::Future<Output = Result<(), EmailQueueError>> + Send;

    /// Dequeues the next available email.
    ///
    /// Returns `(EmailId, Envelope, attempt, token)` where `attempt` is
    /// 1-based and `token` must be passed to `ack` or `nack`.
    fn dequeue(
        &self,
    ) -> impl std::future::Future<
        Output = Result<(EmailId, Envelope, u32, AckToken), EmailQueueError>,
    > + Send;

    /// Acknowledges successful processing or permanent failure -- removes the
    /// item from the queue.
    fn ack(
        &self,
        token: AckToken,
    ) -> impl std::future::Future<Output = Result<(), EmailQueueError>> + Send;

    /// Negatively acknowledges a transient failure. The item will become
    /// visible again after `delay`.
    fn nack(
        &self,
        token: AckToken,
        delay: Duration,
    ) -> impl std::future::Future<Output = Result<(), EmailQueueError>> + Send;
}
