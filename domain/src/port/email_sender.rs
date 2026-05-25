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
}

impl SendError {
    #[must_use]
    pub fn sender_name(&self) -> &SenderName {
        match self {
            Self::Send { sender_name, .. } => sender_name,
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
    /// Returns `SendError::Send` when the underlying transport fails to deliver.
    fn send(
        &self,
        email: OutboundEmail,
    ) -> impl std::future::Future<Output = Result<SenderName, SendError>> + Send;
}
