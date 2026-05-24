use thiserror::Error;

use crate::entity::body::RenderedBody;
use crate::entity::email::RecipientKind;

#[derive(Debug, Error)]
pub enum SendError {
    #[error("email send failed")]
    Send {
        #[source]
        source: anyhow::Error,
    },
}

pub struct OutboundEmail {
    pub sender: String,
    pub subject: Option<String>,
    pub recipients: Vec<(RecipientKind, String)>,
    pub body: RenderedBody,
}

pub trait EmailSender {
    fn send(
        &self,
        email: OutboundEmail,
    ) -> impl std::future::Future<Output = Result<(), SendError>> + Send;
}
