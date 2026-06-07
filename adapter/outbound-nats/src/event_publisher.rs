use anyhow::Context;
use catapulte_domain::entity::lifecycle_event::LifecycleEvent;
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
    serde_json::json!({
        "event_type": event.event_type(),
        "email_id": event.email_id().as_uuid().to_string(),
        "payload": event.payload(),
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

#[cfg(test)]
mod tests {
    use catapulte_domain::entity::email::EmailId;
    use catapulte_domain::entity::error_class::ErrorClass;
    use catapulte_domain::entity::lifecycle_event::LifecycleEvent;
    use catapulte_domain::entity::sender::SenderName;

    use super::event_to_json;

    /// Build the canonical expected body for a given event — shared between the
    /// webhook and NATS contract-lock tests so they are provably identical.
    fn expected_body(event: &LifecycleEvent) -> serde_json::Value {
        serde_json::json!({
            "event_type": event.event_type(),
            "email_id": event.email_id().as_uuid().to_string(),
            "payload": event.payload(),
        })
    }

    /// Contract-lock: Sent event — full JSON body equals canonical shape.
    #[test]
    fn contract_sent_full_body() {
        let id = EmailId::default();
        let event = LifecycleEvent::Sent {
            id,
            sender_name: SenderName::new("primary"),
            correlation_id: Some("corr-sent".to_owned()),
        };
        let expected = serde_json::json!({
            "event_type": "delivery.succeeded",
            "email_id": id.as_uuid().to_string(),
            "payload": {
                "sender_name": "primary",
                "correlation_id": "corr-sent",
            },
        });
        assert_eq!(event_to_json(&event), expected);
        // Confirm parity with the helper used by the webhook contract-lock test.
        assert_eq!(event_to_json(&event), expected_body(&event));
    }

    /// Contract-lock: Failed event — full JSON body equals canonical shape.
    #[test]
    fn contract_failed_full_body() {
        let id = EmailId::default();
        let event = LifecycleEvent::Failed {
            id,
            attempt: 3,
            reason: "smtp error".to_owned(),
            error_class: ErrorClass::Delivery,
            sender_name: Some(SenderName::new("primary")),
            correlation_id: Some("corr-fail".to_owned()),
        };
        let expected = serde_json::json!({
            "event_type": "delivery.failed",
            "email_id": id.as_uuid().to_string(),
            "payload": {
                "attempt": 3,
                "reason": "smtp error",
                "error_class": "delivery",
                "sender_name": "primary",
                "correlation_id": "corr-fail",
            },
        });
        assert_eq!(event_to_json(&event), expected);
        assert_eq!(event_to_json(&event), expected_body(&event));
    }
}
