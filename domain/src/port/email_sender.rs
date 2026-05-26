use thiserror::Error;

use crate::entity::body::RenderedBody;
use crate::entity::email::RecipientKind;
use crate::entity::sender::SenderName;

#[derive(Debug, Error)]
pub enum SendError {
    #[error("email send failed")]
    Send {
        sender_name: SenderName,
        #[source]
        source: anyhow::Error,
    },
    #[error("no route matches sender domain {sender_domain:?}")]
    NoMatchingRoute { sender_domain: String },
}

impl SendError {
    #[must_use]
    pub fn sender_name(&self) -> Option<&SenderName> {
        match self {
            Self::Send { sender_name, .. } => Some(sender_name),
            Self::NoMatchingRoute { .. } => None,
        }
    }

    #[must_use]
    pub fn is_transient(&self) -> bool {
        match self {
            Self::Send { .. } => true,
            Self::NoMatchingRoute { .. } => false,
        }
    }
}

pub struct OutboundEmail {
    pub sender: String,
    pub subject: Option<String>,
    pub recipients: Vec<(RecipientKind, String)>,
    pub body: RenderedBody,
    pub attachments: Vec<crate::entity::attachment::ResolvedAttachment>,
}

pub trait EmailSender: Send + Sync + 'static {
    /// # Errors
    ///
    /// Returns a `SendError` when delivery cannot complete: `SendError::NoMatchingRoute`
    /// when no configured route matches the sender domain, or `SendError::Send` when
    /// all matched transports fail to deliver.
    fn send(
        &self,
        email: OutboundEmail,
    ) -> impl std::future::Future<Output = Result<SenderName, SendError>> + Send;
}
