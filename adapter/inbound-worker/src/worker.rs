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
    pub async fn run<S: WorkerState>(self, state: S) {
        loop {
            match state.email_queue().dequeue().await {
                Ok((id, envelope, attempt, token)) => {
                    process_one(&state, id, envelope, attempt, token).await;
                }
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
