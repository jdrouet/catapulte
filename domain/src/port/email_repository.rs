use thiserror::Error;

use crate::entity::email::EmailId;
use crate::entity::envelope::Envelope;

pub enum SaveResult {
    Created(EmailId),
    Duplicate(EmailId),
}

#[derive(Debug, Error)]
pub enum EmailRepositoryError {
    #[error("email storage failed")]
    Storage {
        #[source]
        source: anyhow::Error,
    },
}

pub trait EmailRepository {
    /// # Errors
    ///
    /// Returns an `EmailRepositoryError` when persistence fails.
    fn save(
        &self,
        id: EmailId,
        envelope: &Envelope,
    ) -> impl std::future::Future<Output = Result<SaveResult, EmailRepositoryError>> + Send;
}
