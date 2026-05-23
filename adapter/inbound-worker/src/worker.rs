use std::time::Duration;

use catapulte_domain::entity::lifecycle_event::LifecycleEvent;
use catapulte_domain::port::email_queue::EmailQueue;
use catapulte_domain::port::event_publisher::EventPublisher;
use catapulte_domain::use_case::process_queued_email::ProcessQueuedEmailUseCase;

pub trait WorkerState: Clone + Send + Sync + 'static {
    fn process_queued_email(&self) -> &impl ProcessQueuedEmailUseCase;
    fn email_queue(&self) -> &impl EmailQueue;
    fn event_publisher(&self) -> &impl EventPublisher;
}

pub struct WorkerConfig {
    pub poll_interval_ms: u64,
}

impl WorkerConfig {
    /// # Errors
    ///
    /// Returns an error if `<prefix>_POLL_INTERVAL_MS` is set but cannot be parsed as u64.
    pub fn from_env(prefix: &str) -> anyhow::Result<Self> {
        use anyhow::Context;
        let key = format!("{prefix}_POLL_INTERVAL_MS");
        let poll_interval_ms = std::env::var(&key)
            .ok()
            .map(|v| v.parse::<u64>().with_context(|| format!("invalid {key}")))
            .transpose()?
            .unwrap_or(1000);
        Ok(Self { poll_interval_ms })
    }

    #[must_use]
    pub fn build(self) -> Worker {
        Worker {
            poll_interval: Duration::from_millis(self.poll_interval_ms),
        }
    }
}

pub struct Worker {
    poll_interval: Duration,
}

impl Worker {
    pub async fn run<S: WorkerState>(self, state: S) {
        let mut interval = tokio::time::interval(self.poll_interval);
        loop {
            interval.tick().await;
            drain_queue(&state).await;
        }
    }
}

async fn process_one<S: WorkerState>(
    state: &S,
    id: catapulte_domain::entity::email::EmailId,
    envelope: catapulte_domain::entity::envelope::Envelope,
) {
    match state.process_queued_email().execute(envelope).await {
        Ok(()) => {
            if let Err(e) = state
                .event_publisher()
                .publish(&LifecycleEvent::Sent { id })
                .await
            {
                tracing::error!(error = %e, "failed to publish sent event");
            }
        }
        Err(e) => {
            tracing::error!(error = %e, email_id = %id.as_uuid(), "failed to process email");
        }
    }
}

async fn drain_queue<S: WorkerState>(state: &S) {
    loop {
        match state.email_queue().dequeue().await {
            Ok(Some((id, envelope))) => process_one(state, id, envelope).await,
            Ok(None) => break,
            Err(e) => {
                tracing::error!(error = %e, "failed to dequeue");
                break;
            }
        }
    }
}
