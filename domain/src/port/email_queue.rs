use thiserror::Error;

use crate::entity::email::EmailId;
use crate::entity::envelope::Envelope;

#[derive(Debug, Error)]
pub enum EmailQueueError {
    #[error("failed to dequeue email")]
    Storage {
        #[source]
        source: anyhow::Error,
    },
}

pub trait EmailQueue {
    fn dequeue(
        &self,
    ) -> impl std::future::Future<Output = Result<Option<(EmailId, Envelope)>, EmailQueueError>> + Send;
}
