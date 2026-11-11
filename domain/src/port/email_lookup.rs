use thiserror::Error;

use crate::entity::email::{EmailId, RecipientKind};
use crate::entity::lifecycle_event::EventContext;

/// What feedback ingestion needs to know about a sent email.
#[derive(Clone, Debug)]
pub struct EmailSummary {
    pub owner: String,
    /// The final envelope recipients: To, Cc and Bcc.
    pub recipients: Vec<(RecipientKind, String)>,
    pub context: EventContext,
}

#[derive(Debug, Error)]
pub enum EmailLookupError {
    #[error("email lookup failed")]
    Storage {
        #[source]
        source: anyhow::Error,
    },
}

pub trait EmailLookup: Send + Sync + 'static {
    /// # Errors
    ///
    /// Returns `EmailLookupError::Storage` when the query fails.
    fn find(
        &self,
        id: EmailId,
    ) -> impl std::future::Future<Output = Result<Option<EmailSummary>, EmailLookupError>> + Send;
}
