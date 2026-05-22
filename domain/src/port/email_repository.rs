use thiserror::Error;

use crate::entity::email::EmailId;
use crate::entity::envelope::Envelope;

#[derive(Debug, Error)]
pub enum EmailRepositoryError {
    #[error("email storage failed")]
    Storage {
        #[source]
        source: anyhow::Error,
    },
}

#[allow(async_fn_in_trait)]
pub trait EmailRepository {
    /// # Errors
    ///
    /// Returns an `EmailRepositoryError` when persistence fails.
    async fn save(&self, id: EmailId, envelope: &Envelope) -> Result<(), EmailRepositoryError>;
}
