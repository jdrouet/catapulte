use catapulte_domain::entity::lifecycle_event::LifecycleEvent;
use catapulte_domain::port::email_queue::{AckToken, EmailQueue};
use catapulte_domain::port::event_publisher::EventPublisher;
use catapulte_domain::use_case::process_queued_email::ProcessQueuedEmailUseCase;

const MAX_ATTEMPTS: u32 = 3;

pub trait WorkerState: Clone + Send + Sync + 'static {
    fn process_queued_email(&self) -> &impl ProcessQueuedEmailUseCase;
    fn email_queue(&self) -> &impl EmailQueue;
    fn event_publisher(&self) -> &impl EventPublisher;
}

pub struct WorkerConfig {}

impl WorkerConfig {
    /// # Errors
    ///
    /// Always succeeds; the signature is kept for consistency with other configs.
    pub fn from_env(_prefix: &str) -> anyhow::Result<Self> {
        Ok(Self {})
    }

    #[must_use]
    pub fn build(self) -> Worker {
        Worker {}
    }
}

pub struct Worker {}

fn backoff(attempt: u32) -> std::time::Duration {
    let secs = (30u64 * (1u64 << attempt.saturating_sub(1))).min(3600);
    std::time::Duration::from_secs(secs)
}

impl Worker {
    pub async fn run<S: WorkerState>(self, state: S, cancel: tokio_util::sync::CancellationToken) {
        loop {
            let result = tokio::select! {
                biased;
                _ = cancel.cancelled() => break,
                result = state.email_queue().dequeue() => result,
            };

            match result {
                Ok((id, envelope, attempt, token)) => {
                    if cancel.is_cancelled() {
                        if let Err(e) = state
                            .email_queue()
                            .nack(token, std::time::Duration::ZERO)
                            .await
                        {
                            tracing::error!(error = %e, "failed to nack message on shutdown");
                        }
                        break;
                    }
                    process_one(&state, id, envelope, attempt, token).await;
                }
                Err(e) => {
                    tracing::error!(error = %e, "failed to dequeue");
                    tokio::select! {
                        biased;
                        _ = cancel.cancelled() => break,
                        _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {}
                    }
                }
            }
        }
        tracing::info!("worker stopped");
    }
}

async fn process_one<S: WorkerState>(
    state: &S,
    id: catapulte_domain::entity::email::EmailId,
    envelope: catapulte_domain::entity::envelope::Envelope,
    attempt: u32,
    token: AckToken,
) {
    if let Err(e) = state
        .event_publisher()
        .publish(&LifecycleEvent::Sending { id, attempt })
        .await
    {
        tracing::error!(error = %e, "failed to publish sending event");
    }

    match state.process_queued_email().execute(envelope).await {
        Ok(sender_name) => {
            if let Err(e) = state.email_queue().ack(token).await {
                tracing::error!(error = %e, email_id = %id.as_uuid(), "failed to ack email");
                return;
            }
            if let Err(e) = state
                .event_publisher()
                .publish(&LifecycleEvent::Sent { id, sender_name })
                .await
            {
                tracing::error!(error = %e, "failed to publish sent event");
            }
        }
        Err(e) => {
            tracing::error!(
                error = %e,
                email_id = %id.as_uuid(),
                attempt,
                "failed to process email"
            );
            let reason = e.to_string();
            let sender_name = e.sender_name().cloned();
            let event = if attempt >= MAX_ATTEMPTS {
                if let Err(ack_err) = state.email_queue().ack(token).await {
                    tracing::error!(error = %ack_err, "failed to ack permanently failed email");
                    return;
                }
                LifecycleEvent::Failed {
                    id,
                    reason,
                    sender_name,
                }
            } else {
                let delay = backoff(attempt);
                if let Err(nack_err) = state.email_queue().nack(token, delay).await {
                    tracing::error!(error = %nack_err, "failed to nack transiently failed email");
                    return;
                }
                LifecycleEvent::Retrying {
                    id,
                    attempt,
                    reason,
                    sender_name,
                }
            };
            if let Err(pub_err) = state.event_publisher().publish(&event).await {
                tracing::error!(error = %pub_err, "failed to publish lifecycle event");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use catapulte_domain::entity::body::{BodySource, Plain};
    use catapulte_domain::entity::email::{EmailId, RecipientKind};
    use catapulte_domain::entity::envelope::Envelope;
    use catapulte_domain::entity::lifecycle_event::LifecycleEvent;
    use catapulte_domain::entity::sender::SenderName;
    use catapulte_domain::port::email_queue::{AckToken, EmailQueue, EmailQueueError};
    use catapulte_domain::port::event_publisher::{EventPublisher, EventPublisherError};
    use catapulte_domain::use_case::process_queued_email::{
        ProcessQueuedEmailError, ProcessQueuedEmailUseCase,
    };

    use super::{Worker, WorkerState, process_one};

    fn sample_envelope() -> Envelope {
        Envelope {
            idempotency_key: None,
            subject: None,
            sender: "sender@example.com".to_owned(),
            recipients: vec![(RecipientKind::To, "to@example.com".to_owned())],
            body: BodySource::Plain(Plain::try_new(Some("hi".to_owned()), None).unwrap()),
            variables: serde_json::Map::new(),
        }
    }

    struct OkProcessor;

    impl ProcessQueuedEmailUseCase for OkProcessor {
        async fn execute(&self, _: Envelope) -> Result<SenderName, ProcessQueuedEmailError> {
            Ok(SenderName::new("sender"))
        }
    }

    #[derive(Clone)]
    struct FailingAckQueue;

    impl EmailQueue for FailingAckQueue {
        async fn enqueue(&self, _: EmailId, _: &Envelope) -> Result<(), EmailQueueError> {
            Ok(())
        }

        async fn dequeue(&self) -> Result<(EmailId, Envelope, u32, AckToken), EmailQueueError> {
            unimplemented!()
        }

        async fn ack(&self, _: AckToken) -> Result<(), EmailQueueError> {
            Err(EmailQueueError::Storage {
                source: anyhow::anyhow!("ack failed"),
            })
        }

        async fn nack(&self, _: AckToken, _: Duration) -> Result<(), EmailQueueError> {
            Ok(())
        }
    }

    #[derive(Clone, Default)]
    struct RecordingPublisher {
        events: Arc<Mutex<Vec<&'static str>>>,
    }

    impl EventPublisher for RecordingPublisher {
        async fn publish(&self, event: &LifecycleEvent) -> Result<(), EventPublisherError> {
            let kind = match event {
                LifecycleEvent::Queued { .. } => "queued",
                LifecycleEvent::Sending { .. } => "sending",
                LifecycleEvent::Sent { .. } => "sent",
                LifecycleEvent::Failed { .. } => "failed",
                LifecycleEvent::Retrying { .. } => "retrying",
            };
            self.events.lock().unwrap().push(kind);
            Ok(())
        }
    }

    #[derive(Clone)]
    struct TestState<Q: Clone + Send + Sync + 'static> {
        queue: Q,
        publisher: RecordingPublisher,
    }

    impl<Q: EmailQueue + Clone + Send + Sync + 'static> WorkerState for TestState<Q> {
        fn process_queued_email(&self) -> &impl ProcessQueuedEmailUseCase {
            &OkProcessor
        }

        fn email_queue(&self) -> &impl EmailQueue {
            &self.queue
        }

        fn event_publisher(&self) -> &impl EventPublisher {
            &self.publisher
        }
    }

    #[derive(Clone)]
    struct NoopQueue;

    impl EmailQueue for NoopQueue {
        async fn enqueue(&self, _: EmailId, _: &Envelope) -> Result<(), EmailQueueError> {
            Ok(())
        }

        async fn dequeue(&self) -> Result<(EmailId, Envelope, u32, AckToken), EmailQueueError> {
            std::future::pending().await
        }

        async fn ack(&self, _: AckToken) -> Result<(), EmailQueueError> {
            Ok(())
        }

        async fn nack(&self, _: AckToken, _: Duration) -> Result<(), EmailQueueError> {
            Ok(())
        }
    }

    #[derive(Clone)]
    struct NoopPublisher;

    impl EventPublisher for NoopPublisher {
        async fn publish(&self, _: &LifecycleEvent) -> Result<(), EventPublisherError> {
            Ok(())
        }
    }

    #[derive(Clone)]
    struct TestStateNoop;

    impl WorkerState for TestStateNoop {
        fn process_queued_email(&self) -> &impl ProcessQueuedEmailUseCase {
            &OkProcessor
        }

        fn email_queue(&self) -> &impl EmailQueue {
            &NoopQueue
        }

        fn event_publisher(&self) -> &impl EventPublisher {
            &NoopPublisher
        }
    }

    #[tokio::test]
    async fn run_stops_when_token_is_cancelled() {
        use tokio_util::sync::CancellationToken;

        let cancel = CancellationToken::new();
        let worker = Worker {};
        let cancel_clone = cancel.clone();

        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            cancel_clone.cancel();
        });

        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            worker.run(TestStateNoop, cancel),
        )
        .await
        .expect("Worker::run must return after cancellation");
    }

    #[tokio::test]
    async fn ack_failure_after_successful_send_suppresses_sent_event() {
        let publisher = RecordingPublisher::default();
        let state = TestState {
            queue: FailingAckQueue,
            publisher: publisher.clone(),
        };
        let id = EmailId::default();
        let token = AckToken::new(vec![0u8; 8]);

        process_one(&state, id, sample_envelope(), 1, token).await;

        let events = publisher.events.lock().unwrap();
        assert!(
            events.contains(&"sending"),
            "sending event should be published before the attempt"
        );
        assert!(
            !events.contains(&"sent"),
            "sent event must not be published when ack fails"
        );
    }
}
