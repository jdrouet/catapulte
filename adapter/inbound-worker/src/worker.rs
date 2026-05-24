use catapulte_domain::entity::lifecycle_event::LifecycleEvent;
use catapulte_domain::port::email_queue::EmailQueue;
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

impl Worker {
    pub async fn run<S: WorkerState>(self, state: S) {
        loop {
            match state.email_queue().dequeue().await {
                Ok((id, envelope, attempt)) => process_one(&state, id, envelope, attempt).await,
                Err(e) => {
                    tracing::error!(error = %e, "failed to dequeue");
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                }
            }
        }
    }
}

async fn process_one<S: WorkerState>(
    state: &S,
    id: catapulte_domain::entity::email::EmailId,
    envelope: catapulte_domain::entity::envelope::Envelope,
    attempt: u32,
) {
    match state.process_queued_email().execute(envelope).await {
        Ok(()) => {
            if let Err(e) = state.email_queue().ack(id).await {
                tracing::error!(error = %e, email_id = %id.as_uuid(), "failed to ack email");
                return;
            }
            if let Err(e) = state
                .event_publisher()
                .publish(&LifecycleEvent::Sent { id })
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
            if attempt >= MAX_ATTEMPTS
                && let Err(ack_err) = state.email_queue().ack(id).await
            {
                tracing::error!(error = %ack_err, "failed to ack permanently failed email");
            }
            if let Err(pub_err) = state
                .event_publisher()
                .publish(&LifecycleEvent::Failed {
                    id,
                    reason: e.to_string(),
                })
                .await
            {
                tracing::error!(error = %pub_err, "failed to publish failed event");
            }
        }
    }
}
