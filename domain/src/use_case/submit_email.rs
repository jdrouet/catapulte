use thiserror::Error;

use crate::entity::email::EmailId;
use crate::entity::envelope::Envelope;
use crate::entity::lifecycle_event::LifecycleEvent;
use crate::port::email_queue::{EmailQueue, EmailQueueError};
use crate::port::email_repository::{EmailRepository, EmailRepositoryError, SaveResult};
use crate::port::event_publisher::EventPublisher;

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
    /// Returns `SubmitEmailError::Persist` when saving the envelope fails.
    /// Returns `SubmitEmailError::Enqueue` when enqueuing fails.
    fn execute(
        &self,
        envelope: Envelope,
    ) -> impl std::future::Future<Output = Result<EmailId, SubmitEmailError>> + Send;
}

pub struct SubmitEmailService<R, Q, P> {
    repository: R,
    queue: Q,
    event_publisher: P,
}

impl<R, Q, P> SubmitEmailService<R, Q, P>
where
    R: EmailRepository,
    Q: EmailQueue,
    P: EventPublisher,
{
    #[must_use]
    pub fn new(repository: R, queue: Q, event_publisher: P) -> Self {
        Self {
            repository,
            queue,
            event_publisher,
        }
    }

    /// # Errors
    ///
    /// Returns `SubmitEmailError::Persist` when saving the envelope fails.
    /// Returns `SubmitEmailError::Enqueue` when enqueuing fails.
    pub async fn execute(&self, envelope: Envelope) -> Result<EmailId, SubmitEmailError> {
        let id = EmailId::default();
        let result = self.repository.save(id, &envelope).await?;
        match result {
            SaveResult::Duplicate(existing_id) => Ok(existing_id),
            SaveResult::Created(id) => {
                // enqueue after persist: a failed enqueue leaves an orphan record recoverable by reconciliation; enqueuing first would let consumers observe an event for a non-existent email
                self.queue.enqueue(id, &envelope).await?;
                if let Err(e) = self
                    .event_publisher
                    .publish(&LifecycleEvent::Queued { id })
                    .await
                {
                    tracing::warn!(error = %e, email_id = %id.as_uuid(), "failed to publish queued event");
                }
                Ok(id)
            }
        }
    }
}

impl<R, Q, P> SubmitEmailUseCase for SubmitEmailService<R, Q, P>
where
    R: EmailRepository + Send + Sync + 'static,
    Q: EmailQueue + Send + Sync + 'static,
    P: EventPublisher + Send + Sync + 'static,
{
    fn execute(
        &self,
        envelope: Envelope,
    ) -> impl std::future::Future<Output = Result<EmailId, SubmitEmailError>> + Send {
        Self::execute(self, envelope)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use crate::entity::body::Plain;
    use crate::entity::email::{EmailId, RecipientKind};
    use crate::entity::envelope::Envelope;
    use crate::entity::lifecycle_event::LifecycleEvent;
    use crate::port::email_queue::{EmailQueue, EmailQueueError};
    use crate::port::email_repository::{EmailRepository, EmailRepositoryError, SaveResult};
    use crate::port::event_publisher::{EventPublisher, EventPublisherError};

    use super::{SubmitEmailError, SubmitEmailService};

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
            attachments: vec![],
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
        async fn save(
            &self,
            id: EmailId,
            envelope: &Envelope,
        ) -> Result<SaveResult, EmailRepositoryError> {
            self.saved
                .lock()
                .unwrap()
                .push((id, envelope.sender.clone()));
            Ok(SaveResult::Created(id))
        }

        async fn list_emails(
            &self,
            _params: crate::port::email_repository::ListEmailsParams,
        ) -> Result<Vec<crate::port::email_repository::EmailRecord>, EmailRepositoryError> {
            Ok(vec![])
        }
    }

    struct FailingRepository;

    #[allow(async_fn_in_trait)]
    impl EmailRepository for FailingRepository {
        async fn save(
            &self,
            _id: EmailId,
            _envelope: &Envelope,
        ) -> Result<SaveResult, EmailRepositoryError> {
            Err(EmailRepositoryError::Storage {
                source: anyhow::anyhow!("storage down"),
            })
        }

        async fn list_emails(
            &self,
            _params: crate::port::email_repository::ListEmailsParams,
        ) -> Result<Vec<crate::port::email_repository::EmailRecord>, EmailRepositoryError> {
            Ok(vec![])
        }
    }

    struct DuplicatingRepository {
        existing_id: EmailId,
    }

    #[allow(async_fn_in_trait)]
    impl EmailRepository for DuplicatingRepository {
        async fn save(
            &self,
            _id: EmailId,
            _envelope: &Envelope,
        ) -> Result<SaveResult, EmailRepositoryError> {
            Ok(SaveResult::Duplicate(self.existing_id))
        }

        async fn list_emails(
            &self,
            _params: crate::port::email_repository::ListEmailsParams,
        ) -> Result<Vec<crate::port::email_repository::EmailRecord>, EmailRepositoryError> {
            Ok(vec![])
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

        async fn dequeue(
            &self,
        ) -> Result<(EmailId, Envelope, u32, crate::port::email_queue::AckToken), EmailQueueError>
        {
            std::future::pending().await
        }

        async fn ack(
            &self,
            _token: crate::port::email_queue::AckToken,
        ) -> Result<(), EmailQueueError> {
            Ok(())
        }

        async fn nack(
            &self,
            _token: crate::port::email_queue::AckToken,
            _delay: std::time::Duration,
        ) -> Result<(), EmailQueueError> {
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

        async fn dequeue(
            &self,
        ) -> Result<(EmailId, Envelope, u32, crate::port::email_queue::AckToken), EmailQueueError>
        {
            std::future::pending().await
        }

        async fn ack(
            &self,
            _token: crate::port::email_queue::AckToken,
        ) -> Result<(), EmailQueueError> {
            Ok(())
        }

        async fn nack(
            &self,
            _token: crate::port::email_queue::AckToken,
            _delay: std::time::Duration,
        ) -> Result<(), EmailQueueError> {
            Ok(())
        }
    }

    #[derive(Clone)]
    struct FakeEventPublisher {
        published: Arc<Mutex<Vec<LifecycleEvent>>>,
    }

    impl FakeEventPublisher {
        fn new() -> Self {
            Self {
                published: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    #[allow(async_fn_in_trait)]
    impl EventPublisher for FakeEventPublisher {
        async fn publish(&self, event: &LifecycleEvent) -> Result<(), EventPublisherError> {
            self.published.lock().unwrap().push(event.clone());
            Ok(())
        }
    }

    struct FailingEventPublisher;

    #[allow(async_fn_in_trait)]
    impl EventPublisher for FailingEventPublisher {
        async fn publish(&self, _event: &LifecycleEvent) -> Result<(), EventPublisherError> {
            Err(EventPublisherError::Publish {
                source: anyhow::anyhow!("publish failed"),
            })
        }
    }

    #[tokio::test]
    async fn execute_persists_the_envelope() {
        let repo = FakeRepository::new();
        let queue = FakeQueue::new();
        let service =
            SubmitEmailService::new(repo.clone(), queue.clone(), FakeEventPublisher::new());
        let id = service
            .execute(make_envelope("sender@example.com"))
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
        let service =
            SubmitEmailService::new(repo.clone(), queue.clone(), FakeEventPublisher::new());
        let id = service
            .execute(make_envelope("sender@example.com"))
            .await
            .unwrap();
        let enqueued = queue.enqueued.lock().unwrap();
        assert_eq!(enqueued.len(), 1);
        assert_eq!(enqueued[0], id);
    }

    #[tokio::test]
    async fn persistence_failure_propagates_as_submit_email_error() {
        let queue = FakeQueue::new();
        let service =
            SubmitEmailService::new(FailingRepository, queue.clone(), FakeEventPublisher::new());
        let err = service
            .execute(make_envelope("sender@example.com"))
            .await
            .unwrap_err();
        assert!(matches!(err, SubmitEmailError::Persist(_)));
        assert!(queue.enqueued.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn enqueue_failure_propagates_as_submit_email_error() {
        let repo = FakeRepository::new();
        let service =
            SubmitEmailService::new(repo.clone(), FailingQueue, FakeEventPublisher::new());
        let err = service
            .execute(make_envelope("sender@example.com"))
            .await
            .unwrap_err();
        assert!(matches!(err, SubmitEmailError::Enqueue(_)));
        assert_eq!(repo.saved.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn execute_returns_a_fresh_id_each_call() {
        let repo = FakeRepository::new();
        let queue = FakeQueue::new();
        let service =
            SubmitEmailService::new(repo.clone(), queue.clone(), FakeEventPublisher::new());
        let id1 = service
            .execute(make_envelope("sender@example.com"))
            .await
            .unwrap();
        let id2 = service
            .execute(make_envelope("sender@example.com"))
            .await
            .unwrap();
        assert_ne!(id1, id2);
    }

    #[tokio::test]
    async fn duplicate_idempotency_key_returns_existing_id_without_enqueue() {
        let existing_id = EmailId::default();
        let queue = FakeQueue::new();
        let service = SubmitEmailService::new(
            DuplicatingRepository { existing_id },
            queue.clone(),
            FakeEventPublisher::new(),
        );
        let returned_id = service
            .execute(make_envelope("sender@example.com"))
            .await
            .unwrap();
        assert_eq!(returned_id, existing_id);
        assert!(queue.enqueued.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn execute_emits_queued_event_after_enqueue() {
        let repo = FakeRepository::new();
        let queue = FakeQueue::new();
        let spy = FakeEventPublisher::new();
        let service = SubmitEmailService::new(repo.clone(), queue.clone(), spy.clone());
        let id = service
            .execute(make_envelope("sender@example.com"))
            .await
            .unwrap();
        let events = spy.published.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0], LifecycleEvent::Queued { id });
    }

    #[tokio::test]
    async fn duplicate_idempotency_key_does_not_emit_queued_event() {
        let existing_id = EmailId::default();
        let queue = FakeQueue::new();
        let spy = FakeEventPublisher::new();
        let service = SubmitEmailService::new(
            DuplicatingRepository { existing_id },
            queue.clone(),
            spy.clone(),
        );
        service
            .execute(make_envelope("sender@example.com"))
            .await
            .unwrap();
        assert!(spy.published.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn publish_failure_does_not_fail_submit() {
        let repo = FakeRepository::new();
        let queue = FakeQueue::new();
        let service = SubmitEmailService::new(repo.clone(), queue.clone(), FailingEventPublisher);
        let result = service.execute(make_envelope("sender@example.com")).await;
        assert!(
            result.is_ok(),
            "publish failure must not propagate as error"
        );
        assert_eq!(
            queue.enqueued.lock().unwrap().len(),
            1,
            "email must still be enqueued"
        );
    }
}
