use thiserror::Error;

use crate::port::email_sender::OutboundEmail;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransportErrorKind {
    /// Worth retrying, here or on another route (4xx, network, connection).
    Transient,
    /// This route refuses the email (auth, policy), another route may accept it.
    PermanentRoute,
    /// The email itself is unacceptable, so no route can deliver it.
    PermanentMessage,
}

#[derive(Debug, Error)]
#[error("{source:#}")]
pub struct TransportError {
    pub kind: TransportErrorKind,
    #[source]
    pub source: anyhow::Error,
}

/// What the relay answered when it accepted a message.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeliveryReceipt {
    /// `"{code} {lines}"`, already capped in length.
    pub smtp_response: Option<String>,
}

pub trait EmailTransport: Send + Sync + 'static {
    /// # Errors
    ///
    /// Returns a `TransportError` when the underlying transport fails to deliver the email.
    fn deliver<'a>(
        &'a self,
        email: &'a OutboundEmail,
    ) -> impl std::future::Future<Output = Result<DeliveryReceipt, TransportError>> + Send + 'a;
}
