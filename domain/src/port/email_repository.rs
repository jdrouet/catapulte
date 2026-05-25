use thiserror::Error;

use crate::entity::email::{EmailId, RecipientKind};
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

pub trait EmailRepository: Send + Sync + 'static {
    /// # Errors
    ///
    /// Returns an `EmailRepositoryError` when persistence fails.
    fn save(
        &self,
        id: EmailId,
        envelope: &Envelope,
    ) -> impl std::future::Future<Output = Result<SaveResult, EmailRepositoryError>> + Send;

    /// # Errors
    ///
    /// Returns an `EmailRepositoryError` when the underlying query fails.
    fn list_emails(
        &self,
        params: ListEmailsParams,
    ) -> impl std::future::Future<Output = Result<Vec<EmailRecord>, EmailRepositoryError>> + Send;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EmailStatus {
    Queued,
    Sent,
    Failed,
}

#[derive(Clone, Debug)]
pub struct ListEmailsParams {
    pub status: Option<EmailStatus>,
    pub after_ms: Option<i64>,
    pub before_ms: Option<i64>,
    pub recipient: Option<String>,
    pub id: Option<EmailId>,
    pub limit: u32,
    pub offset: u32,
}

#[derive(Clone, Debug)]
pub struct EmailRecord {
    pub id: EmailId,
    pub idempotency_key: Option<String>,
    pub subject: Option<String>,
    pub sender: String,
    pub recipients: Vec<(RecipientKind, String)>,
    pub created_at_ms: i64,
    pub status: EmailStatus,
}
