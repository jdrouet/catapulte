use anyhow::Context;
use catapulte_domain::entity::lifecycle_event::LifecycleEvent;
use catapulte_domain::port::event_publisher::{EventPublisher, EventPublisherError};
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

#[derive(Clone)]
pub struct WebhookPublisher {
    client: reqwest::Client,
    url: url::Url,
    secret: Option<String>,
}

impl WebhookPublisher {
    #[must_use]
    pub fn new(client: reqwest::Client, url: url::Url) -> Self {
        Self {
            client,
            url,
            secret: None,
        }
    }

    /// Signs every request with HMAC-SHA256 over `<timestamp>.<body>` using `secret`.
    #[must_use]
    pub fn with_secret(mut self, secret: String) -> Self {
        self.secret = Some(secret);
        self
    }
}

fn sign(secret: &str, timestamp: u64, body: &[u8]) -> String {
    use std::fmt::Write as _;

    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    mac.update(timestamp.to_string().as_bytes());
    mac.update(b".");
    mac.update(body);
    mac.finalize()
        .into_bytes()
        .iter()
        .fold(String::from("sha256="), |mut out, b| {
            let _ = write!(out, "{b:02x}");
            out
        })
}

fn event_to_json(event: &LifecycleEvent) -> serde_json::Value {
    serde_json::json!({
        "event_type": event.event_type(),
        "email_id": event.email_id().as_uuid().to_string(),
        "payload": event.payload(),
    })
}

fn failure_kind(e: &reqwest::Error) -> &'static str {
    if e.is_timeout() {
        "timeout"
    } else if e.is_connect() {
        "connect"
    } else if e.status().is_some_and(|s| s.is_client_error()) {
        "client_error"
    } else if e.status().is_some_and(|s| s.is_server_error()) {
        "server_error"
    } else {
        "request"
    }
}

impl EventPublisher for WebhookPublisher {
    async fn publish(&self, event: &LifecycleEvent) -> Result<(), EventPublisherError> {
        let body = serde_json::to_vec(&event_to_json(event))
            .context("serializing webhook payload")
            .map_err(|source| EventPublisherError::Publish { source })?;
        let mut delay = std::time::Duration::from_millis(100);
        let mut last_err: Option<anyhow::Error> = None;
        for attempt in 0..3u32 {
            if attempt > 0 {
                tokio::time::sleep(delay).await;
                delay = std::time::Duration::from_millis(500);
            }
            let mut request = self
                .client
                .post(self.url.clone())
                .header(reqwest::header::CONTENT_TYPE, "application/json");
            if let Some(secret) = &self.secret {
                // A fresh timestamp per attempt keeps retries inside the receiver's window.
                let ts = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_secs());
                request = request
                    .header("X-Catapulte-Timestamp", ts.to_string())
                    .header("X-Catapulte-Signature", sign(secret, ts, &body));
            }
            match request
                .body(body.clone())
                .send()
                .await
                .and_then(reqwest::Response::error_for_status)
            {
                Ok(_) => return Ok(()),
                Err(e) => {
                    // Remote replies and the URL (may embed a token) stay out of logs.
                    tracing::warn!(
                        email_id = %event.email_id().as_uuid(),
                        attempt,
                        error_kind = failure_kind(&e),
                        "webhook delivery failed"
                    );
                    let context = if e.is_status() {
                        "webhook returned error status"
                    } else {
                        "sending webhook request"
                    };
                    last_err = Some(anyhow::Error::new(e).context(context));
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
    pub secret: Option<String>,
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
        let secret = std::env::var(format!("{prefix}_SECRET"))
            .ok()
            .filter(|v| !v.is_empty());
        Ok(Self {
            url,
            timeout_ms,
            secret,
        })
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
        let publisher = WebhookPublisher::new(client, url);
        Ok(Some(match self.secret {
            Some(secret) => publisher.with_secret(secret),
            None => publisher,
        }))
    }
}

#[cfg(test)]
mod tests {
    use catapulte_domain::entity::email::EmailId;
    use catapulte_domain::entity::lifecycle_event::{EventContext, LifecycleEvent};
    use catapulte_domain::port::event_publisher::EventPublisher;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use hmac::{Hmac, KeyInit, Mac};
    use sha2::Sha256;

    use std::time::Duration;

    use super::{WebhookConfig, WebhookPublisher, failure_kind};

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
                context: EventContext::with_correlation(None),
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
                context: EventContext::with_correlation(None),
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
                context: EventContext::with_correlation(None),
            })
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn publish_failed_includes_attempt_and_correlation_in_payload() {
        use catapulte_domain::entity::error_class::ErrorClass;
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
                error_class: ErrorClass::Delivery,
                failure_category:
                    catapulte_domain::entity::failure_category::FailureCategory::Other,
                sender_name: Some(SenderName::new("test")),
                context: EventContext::with_correlation(Some("corr-xyz".into())),
            })
            .await
            .unwrap();

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(body["payload"]["attempt"], 3);
        assert_eq!(body["payload"]["correlation_id"], "corr-xyz");
        assert_eq!(body["payload"]["error_class"], "delivery");
        assert_eq!(body["payload"]["failure_category"], "other");
    }

    /// Contract-lock: full body for a Sent event matches the canonical shape.
    #[tokio::test]
    async fn contract_sent_full_body() {
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
            .publish(&LifecycleEvent::Sent {
                id,
                sender_name: SenderName::new("primary"),
                context: EventContext::with_correlation(Some("corr-sent".to_owned())),
                smtp_response: Some("250 2.0.0 Ok: queued as 4ABC".to_owned()),
                message_id: "<test@example.com>".to_owned(),
            })
            .await
            .unwrap();

        let requests = server.received_requests().await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        let expected = serde_json::json!({
            "event_type": "delivery.succeeded",
            "email_id": id.as_uuid().to_string(),
            "payload": {
                "sender_name": "primary",
                "correlation_id": "corr-sent",
                "smtp_response": "250 2.0.0 Ok: queued as 4ABC",
                "message_id": "<test@example.com>",
            },
        });
        assert_eq!(body, expected);
    }

    /// Contract-lock: full body for a Failed event matches the canonical shape.
    #[tokio::test]
    async fn contract_failed_full_body() {
        use catapulte_domain::entity::error_class::ErrorClass;
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
                error_class: ErrorClass::Delivery,
                failure_category:
                    catapulte_domain::entity::failure_category::FailureCategory::Other,
                sender_name: Some(SenderName::new("primary")),
                context: EventContext::with_correlation(Some("corr-fail".to_owned())),
            })
            .await
            .unwrap();

        let requests = server.received_requests().await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        let expected = serde_json::json!({
            "event_type": "delivery.failed",
            "email_id": id.as_uuid().to_string(),
            "payload": {
                "attempt": 3,
                "reason": "smtp error",
                "error_class": "delivery",
                "failure_category": "other",
                "sender_name": "primary",
                "correlation_id": "corr-fail",
            },
        });
        assert_eq!(body, expected);
    }

    fn expected_signature(secret: &str, ts: &str, body: &[u8]) -> String {
        use std::fmt::Write as _;

        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(ts.as_bytes());
        mac.update(b".");
        mac.update(body);
        let mut hex = String::new();
        for b in mac.finalize().into_bytes() {
            write!(hex, "{b:02x}").unwrap();
        }
        format!("sha256={hex}")
    }

    fn assert_signed(request: &wiremock::Request, secret: &str) {
        let ts = request
            .headers
            .get("x-catapulte-timestamp")
            .unwrap()
            .to_str()
            .unwrap();
        let signature = request
            .headers
            .get("x-catapulte-signature")
            .unwrap()
            .to_str()
            .unwrap();
        assert_eq!(signature, expected_signature(secret, ts, &request.body));
        assert_eq!(
            request.headers.get("content-type").unwrap(),
            "application/json"
        );
    }

    fn queued() -> LifecycleEvent {
        LifecycleEvent::Queued {
            id: EmailId::default(),
            context: EventContext::with_correlation(None),
        }
    }

    async fn ok_server() -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        server
    }

    #[tokio::test]
    async fn signed_request_can_be_verified_by_receiver() {
        let server = ok_server().await;

        publisher_for(&server)
            .with_secret("s3cret".to_owned())
            .publish(&queued())
            .await
            .unwrap();

        let requests = server.received_requests().await.unwrap();
        assert_signed(&requests[0], "s3cret");
    }

    #[tokio::test]
    async fn config_build_with_secret_delivers_signed_requests() {
        let server = ok_server().await;
        let publisher = WebhookConfig {
            url: Some(url::Url::parse(&server.uri()).unwrap()),
            timeout_ms: 5_000,
            secret: Some("s3cret".to_owned()),
        }
        .build()
        .unwrap()
        .unwrap();

        publisher.publish(&queued()).await.unwrap();

        let requests = server.received_requests().await.unwrap();
        assert_signed(&requests[0], "s3cret");
    }

    #[tokio::test]
    async fn without_secret_no_signature_headers_are_sent() {
        let server = ok_server().await;

        publisher_for(&server).publish(&queued()).await.unwrap();

        let requests = server.received_requests().await.unwrap();
        assert!(!requests[0].headers.contains_key("x-catapulte-timestamp"));
        assert!(!requests[0].headers.contains_key("x-catapulte-signature"));
    }

    #[tokio::test]
    async fn each_retry_is_signed_over_its_own_timestamp() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        publisher_for(&server)
            .with_secret("s3cret".to_owned())
            .publish(&queued())
            .await
            .unwrap();

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 2);
        for request in &requests {
            assert_signed(request, "s3cret");
        }
    }

    const SHORT: Duration = Duration::from_millis(200);

    async fn status_error(status: u16) -> reqwest::Error {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(status))
            .mount(&server)
            .await;
        reqwest::Client::new()
            .post(server.uri())
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap_err()
    }

    #[tokio::test]
    async fn failure_kind_timeout_when_server_never_answers() {
        // The kernel completes the handshake, but nothing ever reads or replies.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let client = reqwest::Client::builder().timeout(SHORT).build().unwrap();

        let err = client
            .post(format!("http://{addr}/"))
            .send()
            .await
            .unwrap_err();

        assert!(!err.is_connect());
        assert_eq!(failure_kind(&err), "timeout");
    }

    #[tokio::test]
    async fn failure_kind_connect_when_port_is_refused() {
        let addr = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        let client = reqwest::Client::builder().timeout(SHORT).build().unwrap();

        let err = client
            .post(format!("http://{addr}/"))
            .send()
            .await
            .unwrap_err();

        assert!(!err.is_timeout());
        assert_eq!(failure_kind(&err), "connect");
    }

    #[tokio::test]
    async fn failure_kind_client_error_on_4xx() {
        assert_eq!(failure_kind(&status_error(404).await), "client_error");
    }

    #[tokio::test]
    async fn failure_kind_server_error_on_5xx() {
        assert_eq!(failure_kind(&status_error(503).await), "server_error");
    }

    #[tokio::test]
    async fn failure_kind_request_when_request_is_invalid() {
        let err = reqwest::Client::new()
            .post("not a url")
            .send()
            .await
            .unwrap_err();

        assert_eq!(failure_kind(&err), "request");
    }

    #[tokio::test]
    async fn failure_kind_prefers_timeout_over_connect() {
        // Once the accept queue is full, Linux drops new SYNs, so the next
        // connect hangs until the connect timeout fires.
        let socket = tokio::net::TcpSocket::new_v4().unwrap();
        socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let listener = socket.listen(1).unwrap();
        let addr = listener.local_addr().unwrap();
        let mut held = Vec::new();
        for _ in 0..16 {
            let connect = tokio::net::TcpStream::connect(addr);
            match tokio::time::timeout(Duration::from_millis(100), connect).await {
                Ok(stream) => held.push(stream.unwrap()),
                Err(_) => break,
            }
        }
        let client = reqwest::Client::builder()
            .connect_timeout(SHORT)
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap();

        let err = client
            .post(format!("http://{addr}/"))
            .send()
            .await
            .unwrap_err();

        assert!(err.is_timeout() && err.is_connect(), "{err:?}");
        assert_eq!(failure_kind(&err), "timeout");
    }
}
