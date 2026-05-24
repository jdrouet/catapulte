use thiserror::Error;

use crate::entity::email::EmailId;
use crate::entity::envelope::Envelope;
use crate::port::email_queue::{EmailQueue, EmailQueueError};
use crate::port::email_repository::{EmailRepository, EmailRepositoryError};

pub struct SubmitParams {}

#[derive(Debug, Error)]
pub enum SubmitEmailError {
    #[error(transparent)]
    Persist(#[from] EmailRepositoryError),
    #[error(transparent)]
    Enqueue(#[from] EmailQueueError),
}

pub trait SubmitEmailUseCase: Send + Sync + 'static {
    /// # Errors
    ///
    /// Returns a `SubmitEmailError` if persistence or enqueuing fails.
    fn execute(
        &self,
        envelope: Envelope,
        params: SubmitParams,
    ) -> impl std::future::Future<Output = Result<EmailId, SubmitEmailError>> + Send;
}

pub struct SubmitEmailService<R, Q> {
    repository: R,
    queue: Q,
}

impl<R, Q> SubmitEmailService<R, Q>
where
    R: EmailRepository,
    Q: EmailQueue,
{
    #[must_use]
    pub fn new(repository: R, queue: Q) -> Self {
        Self { repository, queue }
    }

    /// # Errors
    ///
    /// Returns `SubmitEmailError::Persist` when saving the envelope fails.
    /// Returns `SubmitEmailError::Enqueue` when enqueuing fails.
    pub async fn execute(
        &self,
        envelope: Envelope,
        _params: SubmitParams,
    ) -> Result<EmailId, SubmitEmailError> {
        let id = EmailId::default();
        self.repository.save(id, &envelope).await?;
        // enqueue after persist: a failed enqueue leaves an orphan record recoverable by reconciliation; enqueuing first would let consumers observe an event for a non-existent email
        self.queue.enqueue(id, &envelope).await?;
        Ok(id)
    }
}

impl<R, Q> SubmitEmailUseCase for SubmitEmailService<R, Q>
where
    R: EmailRepository + Send + Sync + 'static,
    Q: EmailQueue + Send + Sync + 'static,
{
    fn execute(
        &self,
        envelope: Envelope,
        params: SubmitParams,
    ) -> impl std::future::Future<Output = Result<EmailId, SubmitEmailError>> + Send {
        Self::execute(self, envelope, params)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use crate::entity::body::Plain;
    use crate::entity::email::{EmailId, RecipientKind};
    use crate::entity::envelope::Envelope;
    use crate::port::email_queue::{EmailQueue, EmailQueueError};
    use crate::port::email_repository::{EmailRepository, EmailRepositoryError};

    use super::{SubmitEmailError, SubmitEmailService, SubmitParams};

    fn make_envelope(sender: &str) -> Envelope {
        Envelope {
            idempotency_key: None,
            subject: None,
            sender: sender.into(),
            recipients: vec![(RecipientKind::To, "to@example.com".into())],
            body: crate::entity::body::BodySource::Plain(
                Plain::try_new(Some("hello".into()), None).unwrap(),
            ),
            variables: serde_json::Map::new(),
        }
    }

    #[derive(Clone)]
    struct FakeRepository {
        saved: Arc<Mutex<Vec<(EmailId, String)>>>,
    }

    impl FakeRepository {
        fn new() -> Self {
            Self {
                saved: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    #[allow(async_fn_in_trait)]
    impl EmailRepository for FakeRepository {
        async fn save(&self, id: EmailId, envelope: &Envelope) -> Result<(), EmailRepositoryError> {
            self.saved
                .lock()
                .unwrap()
                .push((id, envelope.sender.clone()));
            Ok(())
        }
    }

    struct FailingRepository;

    #[allow(async_fn_in_trait)]
    impl EmailRepository for FailingRepository {
        async fn save(
            &self,
            _id: EmailId,
            _envelope: &Envelope,
        ) -> Result<(), EmailRepositoryError> {
            Err(EmailRepositoryError::Storage {
                source: anyhow::anyhow!("storage down"),
            })
        }
    }

    #[derive(Clone)]
    struct FakeQueue {
        enqueued: Arc<Mutex<Vec<EmailId>>>,
    }

    impl FakeQueue {
        fn new() -> Self {
            Self {
                enqueued: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    #[allow(async_fn_in_trait)]
    impl EmailQueue for FakeQueue {
        async fn enqueue(&self, id: EmailId, _envelope: &Envelope) -> Result<(), EmailQueueError> {
            self.enqueued.lock().unwrap().push(id);
            Ok(())
        }

        async fn dequeue(&self) -> Result<(EmailId, Envelope), EmailQueueError> {
            std::future::pending().await
        }

        async fn ack(&self, _id: EmailId) -> Result<(), EmailQueueError> {
            Ok(())
        }
    }

    struct FailingQueue;

    #[allow(async_fn_in_trait)]
    impl EmailQueue for FailingQueue {
        async fn enqueue(&self, _id: EmailId, _envelope: &Envelope) -> Result<(), EmailQueueError> {
            Err(EmailQueueError::Storage {
                source: anyhow::anyhow!("queue down"),
            })
        }

        async fn dequeue(&self) -> Result<(EmailId, Envelope), EmailQueueError> {
            std::future::pending().await
        }

        async fn ack(&self, _id: EmailId) -> Result<(), EmailQueueError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn execute_persists_the_envelope() {
        let repo = FakeRepository::new();
        let queue = FakeQueue::new();
        let service = SubmitEmailService::new(repo.clone(), queue.clone());
        let id = service
            .execute(make_envelope("sender@example.com"), SubmitParams {})
            .await
            .unwrap();
        let saved = repo.saved.lock().unwrap();
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].0, id);
        assert_eq!(saved[0].1, "sender@example.com");
    }

    #[tokio::test]
    async fn execute_enqueues_the_email() {
        let repo = FakeRepository::new();
        let queue = FakeQueue::new();
        let service = SubmitEmailService::new(repo.clone(), queue.clone());
        let id = service
            .execute(make_envelope("sender@example.com"), SubmitParams {})
            .await
            .unwrap();
        let enqueued = queue.enqueued.lock().unwrap();
        assert_eq!(enqueued.len(), 1);
        assert_eq!(enqueued[0], id);
    }

    #[tokio::test]
    async fn persistence_failure_propagates_as_submit_email_error() {
        let queue = FakeQueue::new();
        let service = SubmitEmailService::new(FailingRepository, queue.clone());
        let err = service
            .execute(make_envelope("sender@example.com"), SubmitParams {})
            .await
            .unwrap_err();
        assert!(matches!(err, SubmitEmailError::Persist(_)));
        assert!(queue.enqueued.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn enqueue_failure_propagates_as_submit_email_error() {
        let repo = FakeRepository::new();
        let service = SubmitEmailService::new(repo.clone(), FailingQueue);
        let err = service
            .execute(make_envelope("sender@example.com"), SubmitParams {})
            .await
            .unwrap_err();
        assert!(matches!(err, SubmitEmailError::Enqueue(_)));
        assert_eq!(repo.saved.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn execute_returns_a_fresh_id_each_call() {
        let repo = FakeRepository::new();
        let queue = FakeQueue::new();
        let service = SubmitEmailService::new(repo.clone(), queue.clone());
        let id1 = service
            .execute(make_envelope("sender@example.com"), SubmitParams {})
            .await
            .unwrap();
        let id2 = service
            .execute(make_envelope("sender@example.com"), SubmitParams {})
            .await
            .unwrap();
        assert_ne!(id1, id2);
    }
}
