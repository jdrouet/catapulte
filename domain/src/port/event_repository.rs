use thiserror::Error;

use crate::entity::email::EmailId;

#[derive(Clone, Debug)]
pub struct ListEventsParams {
    pub email_id: Option<EmailId>,
    pub event_type: Option<String>,
    pub after_ms: Option<i64>,
    pub before_ms: Option<i64>,
    pub limit: u32,
    pub offset: u32,
}

#[derive(Clone, Debug)]
pub struct EventRecord {
    pub id: uuid::Uuid,
    pub email_id: EmailId,
    pub event_type: String,
    pub payload: Option<serde_json::Value>,
    pub created_at_ms: i64,
}

#[derive(Debug, Error)]
pub enum EventRepositoryError {
    #[error("event repository error")]
    Storage {
        #[source]
        source: anyhow::Error,
    },
}

pub trait EventRepository: Send + Sync + 'static {
    /// # Errors
    ///
    /// Returns an `EventRepositoryError` when the underlying query fails.
    fn list_events(
        &self,
        params: ListEventsParams,
    ) -> impl std::future::Future<Output = Result<Vec<EventRecord>, EventRepositoryError>> + Send;
}
