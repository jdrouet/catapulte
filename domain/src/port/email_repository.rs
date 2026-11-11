use thiserror::Error;

use crate::entity::attachment::{AttachmentRef, BlobRef};
use crate::entity::email::{EmailId, RecipientKind};
use crate::entity::envelope::Envelope;

/// Where an email stands on its way into the queue. A freshly saved row is
/// `Reserved`; the transitions below are the only ones callers may request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnqueueState {
    Publishable,
    Enqueued,
}

/// State of an existing row found by idempotency key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DuplicateState {
    /// The submission never reached the publish step.
    Reserved,
    /// The submission got past the reservation: the row is publishable,
    /// queued, or predates enqueue states.
    Complete,
}

#[allow(clippy::large_enum_variant)] // one short-lived value per save
pub enum SaveResult {
    Created(EmailId),
    /// An email with the same idempotency key exists. `recovery` holds the
    /// stored envelope when the original publish outcome is unknown and the
    /// caller must publish it again.
    Duplicate {
        id: EmailId,
        state: DuplicateState,
        recovery: Option<Envelope>,
    },
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

    /// Returns every blob ref referenced by any email row (regardless of status).
    ///
    /// # Errors
    ///
    /// Returns `EmailRepositoryError::Storage` when the underlying query fails.
    fn list_all_attachment_blobs(
        &self,
    ) -> impl std::future::Future<Output = Result<Vec<BlobRef>, EmailRepositoryError>> + Send;

    /// # Errors
    ///
    /// Returns an `EmailRepositoryError` when the underlying query fails.
    fn list_emails(
        &self,
        params: ListEmailsParams,
    ) -> impl std::future::Future<Output = Result<Vec<EmailRecord>, EmailRepositoryError>> + Send;

    /// # Errors
    ///
    /// Returns `EmailRepositoryError::Storage` if the row is missing or the
    /// update fails.
    fn set_attachments(
        &self,
        id: EmailId,
        attachments: &[AttachmentRef],
    ) -> impl std::future::Future<Output = Result<(), EmailRepositoryError>> + Send;

    /// # Errors
    ///
    /// Returns `EmailRepositoryError::Storage` if no row of `owner` matches or the
    /// update fails.
    fn set_enqueue_state(
        &self,
        id: EmailId,
        owner: &str,
        state: EnqueueState,
    ) -> impl std::future::Future<Output = Result<(), EmailRepositoryError>> + Send;

    /// # Errors
    ///
    /// Returns `EmailRepositoryError::Storage` when the delete fails for a
    /// reason other than a missing row. A missing row is NOT an error —
    /// delete is idempotent.
    fn delete(
        &self,
        id: EmailId,
    ) -> impl std::future::Future<Output = Result<(), EmailRepositoryError>> + Send;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EmailStatus {
    Queued,
    Sent,
    Failed,
}

#[derive(Clone, Debug)]
pub struct ListEmailsParams {
    /// Only emails carrying this tag.
    pub tag: Option<String>,
    /// Restricts the list to emails submitted by this API key; `None` lists every owner.
    pub owner: Option<String>,
    pub status: Option<EmailStatus>,
    pub after_ms: Option<i64>,
    pub before_ms: Option<i64>,
    pub recipient: Option<String>,
    pub template: Option<String>,
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
    pub tags: Vec<String>,
    pub metadata: std::collections::BTreeMap<String, String>,
}
