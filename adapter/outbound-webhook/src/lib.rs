use anyhow::Context;
use catapulte_domain::entity::lifecycle_event::LifecycleEvent;
use catapulte_domain::entity::sender::SenderName;
use catapulte_domain::port::event_publisher::{EventPublisher, EventPublisherError};

#[derive(Clone)]
pub struct WebhookPublisher {
    client: reqwest::Client,
    url: url::Url,
}

impl WebhookPublisher {
    #[must_use]
    pub fn new(client: reqwest::Client, url: url::Url) -> Self {
        Self { client, url }
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
            sender_name,
            correlation_id,
        }
        | LifecycleEvent::Failed {
            id,
            attempt,
            reason,
            sender_name,
            correlation_id,
        } => (
            id,
            serde_json::json!({
                "attempt": attempt,
                "reason": reason,
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

impl EventPublisher for WebhookPublisher {
    async fn publish(&self, event: &LifecycleEvent) -> Result<(), EventPublisherError> {
        let body = event_to_json(event);
        let mut delay = std::time::Duration::from_millis(100);
        let mut last_err: Option<anyhow::Error> = None;
        for attempt in 0..3u32 {
            if attempt > 0 {
                tokio::time::sleep(delay).await;
                delay = std::time::Duration::from_millis(500);
            }
            match self
                .client
                .post(self.url.clone())
                .json(&body)
                .send()
                .await
                .context("sending webhook request")
                .and_then(|r| {
                    r.error_for_status()
                        .context("webhook returned error status")
                }) {
                Ok(_) => return Ok(()),
                Err(e) => {
                    tracing::warn!(error = %e, attempt, "webhook delivery failed");
                    last_err = Some(e);
                }
            }
        }
        Err(EventPublisherError::Publish {
            source: last_err.unwrap(),
        })
    }
}

pub struct WebhookConfig {
    pub url: Option<url::Url>,
    pub timeout_ms: u64,
}

impl WebhookConfig {
    /// # Errors
    ///
    /// Returns an error if the URL env var is set but unparseable.
    pub fn from_env(prefix: &str) -> anyhow::Result<Self> {
        let url = std::env::var(format!("{prefix}_URL"))
            .ok()
            .map(|v| url::Url::parse(&v).context("parsing webhook URL"))
            .transpose()?;
        let timeout_ms = std::env::var(format!("{prefix}_TIMEOUT_MS"))
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(5_000u64);
        Ok(Self { url, timeout_ms })
    }

    /// # Errors
    ///
    /// Returns an error if the reqwest client cannot be built.
    pub fn build(self) -> anyhow::Result<Option<WebhookPublisher>> {
        let Some(url) = self.url else {
            return Ok(None);
        };
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(self.timeout_ms))
            .build()
            .context("building reqwest client")?;
        Ok(Some(WebhookPublisher::new(client, url)))
    }
}

#[cfg(test)]
mod tests {
    use catapulte_domain::entity::email::EmailId;
    use catapulte_domain::entity::lifecycle_event::LifecycleEvent;
    use catapulte_domain::port::event_publisher::EventPublisher;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::WebhookPublisher;

    fn publisher_for(server: &MockServer) -> WebhookPublisher {
        let url = url::Url::parse(&server.uri()).unwrap();
        WebhookPublisher::new(reqwest::Client::new(), url)
    }

    #[tokio::test]
    async fn publish_queued_posts_json_to_webhook() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let publisher = publisher_for(&server);
        let id = EmailId::default();
        publisher
            .publish(&LifecycleEvent::Queued {
                id,
                correlation_id: None,
            })
            .await
            .unwrap();

        server.verify().await;
    }

    #[tokio::test]
    async fn publish_retries_on_server_error_and_succeeds() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(500))
            .up_to_n_times(2)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let publisher = publisher_for(&server);
        let id = EmailId::default();
        publisher
            .publish(&LifecycleEvent::Queued {
                id,
                correlation_id: None,
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn publish_returns_error_after_three_failures() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let publisher = publisher_for(&server);
        let id = EmailId::default();
        let result = publisher
            .publish(&LifecycleEvent::Queued {
                id,
                correlation_id: None,
            })
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn publish_failed_includes_attempt_and_correlation_in_payload() {
        use catapulte_domain::entity::sender::SenderName;

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let publisher = publisher_for(&server);
        let id = EmailId::default();
        publisher
            .publish(&LifecycleEvent::Failed {
                id,
                attempt: 3,
                reason: "smtp error".to_owned(),
                sender_name: Some(SenderName::new("test")),
                correlation_id: Some("corr-xyz".into()),
            })
            .await
            .unwrap();

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(body["payload"]["attempt"], 3);
        assert_eq!(body["payload"]["correlation_id"], "corr-xyz");
    }
}
