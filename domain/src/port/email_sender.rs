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

#[allow(async_fn_in_trait)]
pub trait EmailSender {
    /// # Errors
    ///
    /// Returns a `SendError` when the email cannot be delivered.
    async fn send(
        &self,
        sender: &str,
        recipients: &[(RecipientKind, String)],
        body: &RenderedBody,
    ) -> Result<(), SendError>;
}
