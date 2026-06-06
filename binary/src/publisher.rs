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
                sr?;
                if let Err(e) = wr {
                    tracing::warn!(error = %e, "webhook event delivery failed");
                }
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
                sr?;
                if let Err(e) = nr {
                    tracing::warn!(error = %e, "NATS event delivery failed");
                }
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
                sr?;
                if let Err(e) = wr {
                    tracing::warn!(error = %e, "webhook event delivery failed");
                }
                if let Err(e) = nr {
                    tracing::warn!(error = %e, "NATS event delivery failed");
                }
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
