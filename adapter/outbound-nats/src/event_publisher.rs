use anyhow::Context;
use catapulte_domain::entity::lifecycle_event::LifecycleEvent;
use catapulte_domain::entity::sender::SenderName;
use catapulte_domain::port::event_publisher::{EventPublisher, EventPublisherError};

#[derive(Clone)]
pub struct NatsEventPublisher {
    client: async_nats::Client,
    subject: String,
}

impl NatsEventPublisher {
    #[must_use]
    pub fn new(client: async_nats::Client, subject: String) -> Self {
        Self { client, subject }
    }
}

fn event_to_json(event: &LifecycleEvent) -> serde_json::Value {
    let (email_id, extra) = match event {
        LifecycleEvent::Queued { id, correlation_id } => {
            (id, serde_json::json!({ "correlation_id": correlation_id }))
        }
        LifecycleEvent::Sending {
            id,
            attempt,
            correlation_id,
        } => (
            id,
            serde_json::json!({ "attempt": attempt, "correlation_id": correlation_id }),
        ),
        LifecycleEvent::Sent {
            id,
            sender_name,
            correlation_id,
        } => (
            id,
            serde_json::json!({
                "sender_name": sender_name.as_str(),
                "correlation_id": correlation_id,
            }),
        ),
        LifecycleEvent::Retrying {
            id,
            attempt,
            reason,
            error_class,
            sender_name,
            correlation_id,
        }
        | LifecycleEvent::Failed {
            id,
            attempt,
            reason,
            error_class,
            sender_name,
            correlation_id,
        } => (
            id,
            serde_json::json!({
                "attempt": attempt,
                "reason": reason,
                "error_class": error_class.as_str(),
                "sender_name": sender_name.as_ref().map(SenderName::as_str),
                "correlation_id": correlation_id,
            }),
        ),
    };
    serde_json::json!({
        "event_type": event.event_type(),
        "email_id": email_id.as_uuid().to_string(),
        "payload": extra,
    })
}

impl EventPublisher for NatsEventPublisher {
    async fn publish(&self, event: &LifecycleEvent) -> Result<(), EventPublisherError> {
        let body = serde_json::to_vec(&event_to_json(event))
            .context("serializing event")
            .map_err(|source| EventPublisherError::Publish { source })?;
        self.client
            .publish(self.subject.clone(), body.into())
            .await
            .context("publishing event to NATS")
            .map_err(|source| EventPublisherError::Publish { source })?;
        Ok(())
    }
}

pub struct NatsEventConfig {
    pub url: Option<String>,
    pub subject: String,
}

impl NatsEventConfig {
    /// # Errors
    ///
    /// Never fails; env vars are optional.
    pub fn from_env(prefix: &str) -> anyhow::Result<Self> {
        let url = std::env::var(format!("{prefix}_URL")).ok();
        let subject = std::env::var(format!("{prefix}_SUBJECT"))
            .unwrap_or_else(|_| "catapulte.lifecycle".to_owned());
        Ok(Self { url, subject })
    }

    /// # Errors
    ///
    /// Returns an error if the NATS connection fails.
    pub async fn build(self) -> anyhow::Result<Option<NatsEventPublisher>> {
        let Some(url) = self.url else {
            return Ok(None);
        };
        let client = async_nats::connect(&url)
            .await
            .context("connecting to NATS for event publisher")?;
        Ok(Some(NatsEventPublisher::new(client, self.subject)))
    }
}
