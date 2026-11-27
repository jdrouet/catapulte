use catapulte_domain::entity::lifecycle_event::LifecycleEvent;
use catapulte_domain::port::event_publisher::{EventPublisher, EventPublisherError};
use catapulte_outbound_nats::event_publisher::{NatsEventConfig, NatsEventPublisher};
use catapulte_outbound_webhook::{WebhookConfig, WebhookPublisher};
use tracing::Instrument as _;

use crate::storage::StorageAdapter;

#[derive(Clone)]
pub(crate) enum PublisherAdapter {
    Storage(StorageAdapter),
    StorageWebhook(StorageAdapter, WebhookPublisher),
    StorageNats(StorageAdapter, NatsEventPublisher),
    StorageBoth(StorageAdapter, WebhookPublisher, NatsEventPublisher),
}

impl EventPublisher for PublisherAdapter {
    async fn publish(&self, event: &LifecycleEvent) -> Result<(), EventPublisherError> {
        match self {
            Self::Storage(s) => {
                let span =
                    tracing::info_span!("publisher.storage", outcome = tracing::field::Empty);
                let result = s.publish(event).instrument(span.clone()).await;
                span.record("outcome", if result.is_ok() { "ok" } else { "error" });
                result
            }
            Self::StorageWebhook(s, w) => {
                let storage_span =
                    tracing::info_span!("publisher.storage", outcome = tracing::field::Empty);
                let webhook_span =
                    tracing::info_span!("publisher.webhook", outcome = tracing::field::Empty);
                let (sr, wr) = tokio::join!(
                    s.publish(event).instrument(storage_span.clone()),
                    w.publish(event).instrument(webhook_span.clone()),
                );
                storage_span.record("outcome", if sr.is_ok() { "ok" } else { "error" });
                webhook_span.record("outcome", if wr.is_ok() { "ok" } else { "error" });
                if wr.is_err() {
                    tracing::warn!(
                        email_id = %event.email_id().as_uuid(),
                        event = event.event_type(),
                        "webhook event delivery failed"
                    );
                }
                sr?;
                Ok(())
            }
            Self::StorageNats(s, n) => {
                let storage_span =
                    tracing::info_span!("publisher.storage", outcome = tracing::field::Empty);
                let nats_span =
                    tracing::info_span!("publisher.nats", outcome = tracing::field::Empty);
                let (sr, nr) = tokio::join!(
                    s.publish(event).instrument(storage_span.clone()),
                    n.publish(event).instrument(nats_span.clone()),
                );
                storage_span.record("outcome", if sr.is_ok() { "ok" } else { "error" });
                nats_span.record("outcome", if nr.is_ok() { "ok" } else { "error" });
                if nr.is_err() {
                    tracing::warn!(
                        email_id = %event.email_id().as_uuid(),
                        event = event.event_type(),
                        "NATS event delivery failed"
                    );
                }
                sr?;
                Ok(())
            }
            Self::StorageBoth(s, w, n) => {
                let storage_span =
                    tracing::info_span!("publisher.storage", outcome = tracing::field::Empty);
                let webhook_span =
                    tracing::info_span!("publisher.webhook", outcome = tracing::field::Empty);
                let nats_span =
                    tracing::info_span!("publisher.nats", outcome = tracing::field::Empty);
                let (sr, wr, nr) = tokio::join!(
                    s.publish(event).instrument(storage_span.clone()),
                    w.publish(event).instrument(webhook_span.clone()),
                    n.publish(event).instrument(nats_span.clone()),
                );
                storage_span.record("outcome", if sr.is_ok() { "ok" } else { "error" });
                webhook_span.record("outcome", if wr.is_ok() { "ok" } else { "error" });
                nats_span.record("outcome", if nr.is_ok() { "ok" } else { "error" });
                if wr.is_err() {
                    tracing::warn!(
                        email_id = %event.email_id().as_uuid(),
                        event = event.event_type(),
                        "webhook event delivery failed"
                    );
                }
                if nr.is_err() {
                    tracing::warn!(
                        email_id = %event.email_id().as_uuid(),
                        event = event.event_type(),
                        "NATS event delivery failed"
                    );
                }
                sr?;
                Ok(())
            }
        }
    }
}

pub struct PublisherAdapterConfig {
    webhook: WebhookConfig,
    nats_events: NatsEventConfig,
}

impl PublisherAdapterConfig {
    #[must_use]
    pub fn storage_only() -> Self {
        Self {
            webhook: WebhookConfig {
                url: None,
                timeout_ms: 5_000,
                secret: None,
            },
            nats_events: NatsEventConfig {
                url: None,
                subject: "catapulte.lifecycle".to_owned(),
            },
        }
    }

    #[must_use]
    pub fn with_nats_events(url: String, subject: String) -> Self {
        Self {
            webhook: WebhookConfig {
                url: None,
                timeout_ms: 5_000,
                secret: None,
            },
            nats_events: NatsEventConfig {
                url: Some(url),
                subject,
            },
        }
    }

    /// # Errors
    ///
    /// Returns an error if any sub-config fails to load from env.
    pub fn from_env() -> anyhow::Result<Self> {
        let webhook = WebhookConfig::from_env("CATAPULTE_WEBHOOK")?;
        let nats_events = NatsEventConfig::from_env("CATAPULTE_NATS_EVENTS")?;
        Ok(Self {
            webhook,
            nats_events,
        })
    }

    /// # Errors
    ///
    /// Returns an error if an outbound connection fails.
    pub(crate) async fn build(self, storage: StorageAdapter) -> anyhow::Result<PublisherAdapter> {
        let webhook = self.webhook.build()?;
        let nats = self.nats_events.build().await?;
        let adapter = match (webhook, nats) {
            (None, None) => PublisherAdapter::Storage(storage),
            (Some(w), None) => PublisherAdapter::StorageWebhook(storage, w),
            (None, Some(n)) => PublisherAdapter::StorageNats(storage, n),
            (Some(w), Some(n)) => PublisherAdapter::StorageBoth(storage, w, n),
        };
        Ok(adapter)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use catapulte_domain::entity::body::{BodySource, Plain};
    use catapulte_domain::entity::email::{EmailId, RecipientKind};
    use catapulte_domain::entity::envelope::Envelope;
    use catapulte_domain::entity::lifecycle_event::{EventContext, LifecycleEvent};
    use catapulte_domain::port::email_repository::EmailRepository;
    use catapulte_domain::port::event_publisher::EventPublisher;
    use catapulte_outbound_webhook::WebhookPublisher;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::PublisherAdapter;
    use crate::storage::StorageAdapter;

    #[derive(Clone, Default)]
    struct SharedBuf(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for SharedBuf {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    async fn fresh_sqlite() -> (StorageAdapter, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("test.db");
        let config = catapulte_outbound_sqlite::SqliteConfig {
            url: format!("sqlite:{}", db_path.display()),
        };
        let adapter = config.build().await.expect("sqlite build");
        adapter.migrate().await.expect("sqlite migrate");
        (StorageAdapter::Sqlite(adapter), dir)
    }

    // Capture needs a process-global subscriber installed before any other test hits the
    // callsites, so it runs in a child process of its own.
    #[test]
    fn webhook_failure_never_leaks_remote_reply_or_url() {
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "publisher::tests::webhook_failure_capture_in_isolated_process",
                "--ignored",
                "--test-threads=1",
                "--nocapture",
            ])
            .output()
            .unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(out.status.success(), "child failed: {text}");
        assert!(text.contains("1 passed"), "child ran no test: {text}");
    }

    #[tokio::test]
    #[ignore = "run in its own process by webhook_failure_never_leaks_remote_reply_or_url"]
    async fn webhook_failure_capture_in_isolated_process() {
        let buf = SharedBuf::default();
        let writer = buf.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(move || writer.clone())
            .with_ansi(false)
            .with_max_level(tracing::Level::TRACE)
            .with_span_events(tracing_subscriber::fmt::format::FmtSpan::FULL)
            .finish();
        tracing::subscriber::set_global_default(subscriber).unwrap();

        let (storage, _dir) = fresh_sqlite().await;
        let id = EmailId::default();
        let envelope = Envelope::new(
            "sender@example.com",
            vec![(RecipientKind::To, "to@example.com".to_owned())],
            BodySource::Plain(Plain::try_new(Some("hi".to_owned()), None).unwrap()),
        );
        storage.save(id, &envelope).await.unwrap();

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/hook/URLMARKER-857"))
            .respond_with(
                ResponseTemplate::new(500).set_body_string("BODYMARKER-857 <victim@example.com>"),
            )
            .expect(3)
            .mount(&server)
            .await;
        let url = reqwest::Url::parse(&format!("{}/hook/URLMARKER-857", server.uri())).unwrap();
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let publisher =
            PublisherAdapter::StorageWebhook(storage, WebhookPublisher::new(client, url));

        publisher
            .publish(&LifecycleEvent::Queued {
                id,
                context: EventContext::with_correlation(None),
            })
            .await
            .unwrap();
        server.verify().await;

        let logs = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
        for marker in ["BODYMARKER-857", "URLMARKER-857", "victim@example.com"] {
            assert!(!logs.contains(marker), "{marker} leaked: {logs}");
        }
        let id = id.as_uuid().to_string();
        assert_eq!(
            logs.lines()
                .filter(|l| l.contains("webhook delivery failed")
                    && l.contains(&id)
                    && l.contains("error_kind=\"server_error\""))
                .count(),
            3,
            "expected three webhook delivery failures: {logs}"
        );
        assert_eq!(
            logs.lines()
                .filter(|l| l.contains("webhook event delivery failed") && l.contains(&id))
                .count(),
            1,
            "expected one event delivery failure: {logs}"
        );
    }
}
