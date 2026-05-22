use thiserror::Error;

use crate::entity::email::EmailId;
use crate::entity::envelope::Envelope;
use crate::entity::lifecycle_event::LifecycleEvent;
use crate::port::email_repository::{EmailRepository, EmailRepositoryError};
use crate::port::event_publisher::{EventPublisher, EventPublisherError};

pub struct SubmitParams {}

#[derive(Debug, Error)]
pub enum SubmitEmailError {
    #[error(transparent)]
    Persist(#[from] EmailRepositoryError),
    #[error(transparent)]
    Publish(#[from] EventPublisherError),
}

pub struct SubmitEmailService<R, P> {
    repository: R,
    publisher: P,
}

impl<R, P> SubmitEmailService<R, P>
where
    R: EmailRepository,
    P: EventPublisher,
{
    #[must_use]
    pub fn new(repository: R, publisher: P) -> Self {
        Self {
            repository,
            publisher,
        }
    }

    /// # Errors
    ///
    /// Returns `SubmitEmailError::Persist` when saving the envelope fails.
    /// Returns `SubmitEmailError::Publish` when emitting the queued event fails.
    pub async fn execute(
        &self,
        envelope: Envelope,
        _params: SubmitParams,
    ) -> Result<EmailId, SubmitEmailError> {
        let id = EmailId::default();
        self.repository.save(id, &envelope).await?;
        // publish after persist: a failed publish leaves an orphan record recoverable by reconciliation; publishing first would let consumers observe an event for a non-existent email
        self.publisher
            .publish(&LifecycleEvent::Queued { id })
            .await?;
        Ok(id)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use crate::entity::body::Plain;
    use crate::entity::email::{EmailId, RecipientKind};
    use crate::entity::envelope::Envelope;
    use crate::entity::lifecycle_event::LifecycleEvent;
    use crate::port::email_repository::{EmailRepository, EmailRepositoryError};
    use crate::port::event_publisher::{EventPublisher, EventPublisherError};

    use super::{SubmitEmailError, SubmitEmailService, SubmitParams};

    fn make_envelope(sender: &str) -> Envelope {
        Envelope {
            idempotency_key: None,
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
    struct FakePublisher {
        published: Arc<Mutex<Vec<LifecycleEvent>>>,
    }

    impl FakePublisher {
        fn new() -> Self {
            Self {
                published: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    #[allow(async_fn_in_trait)]
    impl EventPublisher for FakePublisher {
        async fn publish(&self, event: &LifecycleEvent) -> Result<(), EventPublisherError> {
            self.published.lock().unwrap().push(event.clone());
            Ok(())
        }
    }

    struct FailingPublisher;

    #[allow(async_fn_in_trait)]
    impl EventPublisher for FailingPublisher {
        async fn publish(&self, _event: &LifecycleEvent) -> Result<(), EventPublisherError> {
            Err(EventPublisherError::Publish {
                source: anyhow::anyhow!("publisher down"),
            })
        }
    }

    #[tokio::test]
    async fn execute_persists_the_envelope() {
        let repo = FakeRepository::new();
        let publisher = FakePublisher::new();
        let service = SubmitEmailService::new(repo.clone(), publisher.clone());
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
    async fn execute_emits_a_queued_event_with_the_returned_id() {
        let repo = FakeRepository::new();
        let publisher = FakePublisher::new();
        let service = SubmitEmailService::new(repo.clone(), publisher.clone());
        let id = service
            .execute(make_envelope("sender@example.com"), SubmitParams {})
            .await
            .unwrap();
        let published = publisher.published.lock().unwrap();
        assert_eq!(published.len(), 1);
        assert_eq!(published[0], LifecycleEvent::Queued { id });
    }

    #[tokio::test]
    async fn persistence_failure_propagates_as_submit_email_error() {
        let publisher = FakePublisher::new();
        let service = SubmitEmailService::new(FailingRepository, publisher.clone());
        let err = service
            .execute(make_envelope("sender@example.com"), SubmitParams {})
            .await
            .unwrap_err();
        assert!(matches!(err, SubmitEmailError::Persist(_)));
        assert!(publisher.published.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn publish_failure_propagates_as_submit_email_error() {
        let repo = FakeRepository::new();
        let service = SubmitEmailService::new(repo.clone(), FailingPublisher);
        let err = service
            .execute(make_envelope("sender@example.com"), SubmitParams {})
            .await
            .unwrap_err();
        assert!(matches!(err, SubmitEmailError::Publish(_)));
        assert_eq!(repo.saved.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn execute_returns_a_fresh_id_each_call() {
        let repo = FakeRepository::new();
        let publisher = FakePublisher::new();
        let service = SubmitEmailService::new(repo.clone(), publisher.clone());
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
