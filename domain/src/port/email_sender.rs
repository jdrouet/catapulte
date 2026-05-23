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

pub trait EmailSender {
    /// # Errors
    ///
    /// Returns a `SendError` when the email cannot be delivered.
    fn send(
        &self,
        sender: &str,
        recipients: &[(RecipientKind, String)],
        body: &RenderedBody,
    ) -> impl std::future::Future<Output = Result<(), SendError>> + Send;
}
