use thiserror::Error;

use crate::entity::email::EmailId;
use crate::entity::lifecycle_event::LifecycleEvent;
use crate::port::email_repository::{CancelOutcome, EmailCancellation, EmailRepositoryError};
use crate::port::event_publisher::EventPublisher;

#[derive(Debug, Error)]
pub enum CancelEmailError {
    #[error(transparent)]
    Repository(#[from] EmailRepositoryError),
}

pub trait CancelEmailUseCase: Send + Sync + 'static {
    /// Cancels a scheduled email no worker has touched. `owner` of `None` matches any email.
    ///
    /// # Errors
    ///
    /// Returns `CancelEmailError::Repository` when the cancellation fails.
    fn execute(
        &self,
        id: EmailId,
        owner: Option<String>,
    ) -> impl std::future::Future<Output = Result<CancelOutcome, CancelEmailError>> + Send;
}

pub struct CancelEmailService<R, P> {
    repository: R,
    event_publisher: P,
}

impl<R, P> CancelEmailService<R, P> {
    pub fn new(repository: R, event_publisher: P) -> Self {
        Self {
            repository,
            event_publisher,
        }
    }
}

impl<R: EmailCancellation, P: EventPublisher> CancelEmailService<R, P> {
    async fn execute_inner(
        &self,
        id: EmailId,
        owner: Option<String>,
    ) -> Result<CancelOutcome, CancelEmailError> {
        let outcome = self.repository.cancel(id, owner.as_deref()).await?;
        if let CancelOutcome::Cancelled { context } = &outcome
            && let Err(e) = self
                .event_publisher
                .publish(&LifecycleEvent::Cancelled {
                    id,
                    context: context.clone(),
                })
                .await
        {
            tracing::warn!(error = %e, email_id = %id.as_uuid(), "failed to publish cancelled event");
        }
        Ok(outcome)
    }
}

impl<R: EmailCancellation, P: EventPublisher> CancelEmailUseCase for CancelEmailService<R, P> {
    fn execute(
        &self,
        id: EmailId,
        owner: Option<String>,
    ) -> impl std::future::Future<Output = Result<CancelOutcome, CancelEmailError>> + Send {
        self.execute_inner(id, owner)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::{CancelEmailError, CancelEmailService, CancelEmailUseCase};
    use crate::entity::email::EmailId;
    use crate::entity::lifecycle_event::{EventContext, LifecycleEvent};
    use crate::port::email_repository::{
        CancelOutcome, DeliveryStart, EmailCancellation, EmailRepositoryError,
    };
    use crate::port::event_publisher::{EventPublisher, EventPublisherError};

    struct FakeCancellation {
        outcome: CancelOutcome,
        owners: Arc<Mutex<Vec<Option<String>>>>,
    }

    impl EmailCancellation for FakeCancellation {
        async fn cancel(
            &self,
            _id: EmailId,
            owner: Option<&str>,
        ) -> Result<CancelOutcome, EmailRepositoryError> {
            self.owners.lock().unwrap().push(owner.map(str::to_owned));
            Ok(self.outcome.clone())
        }

        async fn start_delivery(
            &self,
            _id: EmailId,
        ) -> Result<DeliveryStart, EmailRepositoryError> {
            unimplemented!()
        }
    }

    struct FailingCancellation;

    impl EmailCancellation for FailingCancellation {
        async fn cancel(
            &self,
            _id: EmailId,
            _owner: Option<&str>,
        ) -> Result<CancelOutcome, EmailRepositoryError> {
            Err(EmailRepositoryError::Storage {
                source: anyhow::anyhow!("db down"),
            })
        }

        async fn start_delivery(
            &self,
            _id: EmailId,
        ) -> Result<DeliveryStart, EmailRepositoryError> {
            unimplemented!()
        }
    }

    #[derive(Clone, Default)]
    struct Spy {
        events: Arc<Mutex<Vec<LifecycleEvent>>>,
        fail: bool,
    }

    impl EventPublisher for Spy {
        async fn publish(&self, event: &LifecycleEvent) -> Result<(), EventPublisherError> {
            self.events.lock().unwrap().push(event.clone());
            if self.fail {
                return Err(EventPublisherError::Publish {
                    source: anyhow::anyhow!("bus down"),
                });
            }
            Ok(())
        }
    }

    fn service(outcome: CancelOutcome, spy: &Spy) -> CancelEmailService<FakeCancellation, Spy> {
        CancelEmailService::new(
            FakeCancellation {
                outcome,
                owners: Arc::default(),
            },
            spy.clone(),
        )
    }

    #[tokio::test]
    async fn cancelled_publishes_exactly_one_event_with_the_context() {
        let spy = Spy::default();
        let context = EventContext::with_correlation(Some("c".to_owned()));
        let id = EmailId::default();

        let outcome = service(
            CancelOutcome::Cancelled {
                context: context.clone(),
            },
            &spy,
        )
        .execute(id, None)
        .await
        .unwrap();

        assert_eq!(
            outcome,
            CancelOutcome::Cancelled {
                context: context.clone()
            }
        );
        assert_eq!(
            *spy.events.lock().unwrap(),
            vec![LifecycleEvent::Cancelled { id, context }]
        );
    }

    #[tokio::test]
    async fn other_outcomes_publish_nothing() {
        for outcome in [
            CancelOutcome::AlreadyCancelled,
            CancelOutcome::NotCancellable,
            CancelOutcome::NotFound,
        ] {
            let spy = Spy::default();
            let got = service(outcome.clone(), &spy)
                .execute(EmailId::default(), None)
                .await
                .unwrap();
            assert_eq!(got, outcome);
            assert!(spy.events.lock().unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn a_publish_failure_does_not_change_the_outcome() {
        let spy = Spy {
            fail: true,
            ..Spy::default()
        };

        let outcome = service(
            CancelOutcome::Cancelled {
                context: EventContext::default(),
            },
            &spy,
        )
        .execute(EmailId::default(), None)
        .await
        .unwrap();

        assert!(matches!(outcome, CancelOutcome::Cancelled { .. }));
    }

    #[tokio::test]
    async fn the_owner_is_forwarded() {
        let spy = Spy::default();
        let svc = service(CancelOutcome::NotFound, &spy);

        svc.execute(EmailId::default(), Some("a".to_owned()))
            .await
            .unwrap();
        svc.execute(EmailId::default(), None).await.unwrap();

        assert_eq!(
            *svc.repository.owners.lock().unwrap(),
            vec![Some("a".to_owned()), None]
        );
    }

    #[tokio::test]
    async fn a_repository_failure_is_an_error_and_publishes_nothing() {
        let spy = Spy::default();
        let svc = CancelEmailService::new(FailingCancellation, spy.clone());

        let err = svc.execute(EmailId::default(), None).await.unwrap_err();

        assert!(matches!(err, CancelEmailError::Repository(_)));
        assert!(spy.events.lock().unwrap().is_empty());
    }
}
