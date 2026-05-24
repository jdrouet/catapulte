use thiserror::Error;

use crate::entity::email::EmailId;
use crate::entity::envelope::Envelope;

#[derive(Debug, Error)]
pub enum EmailQueueError {
    #[error("email queue error")]
    Storage {
        #[source]
        source: anyhow::Error,
    },
}

pub trait EmailQueue {
    fn enqueue(
        &self,
        id: EmailId,
        envelope: &Envelope,
    ) -> impl std::future::Future<Output = Result<(), EmailQueueError>> + Send;

    fn dequeue(
        &self,
    ) -> impl std::future::Future<Output = Result<(EmailId, Envelope), EmailQueueError>> + Send;

    fn ack(
        &self,
        id: EmailId,
    ) -> impl std::future::Future<Output = Result<(), EmailQueueError>> + Send;
}
