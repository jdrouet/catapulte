use catapulte_domain::entity::lifecycle_event::LifecycleEvent;
use catapulte_domain::port::event_publisher::{EventPublisher, EventPublisherError};
use catapulte_outbound_nats::event_publisher::{NatsEventConfig, NatsEventPublisher};
use catapulte_outbound_webhook::{WebhookConfig, WebhookPublisher};

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
            Self::Storage(s) => s.publish(event).await,
            Self::StorageWebhook(s, w) => {
                let (sr, wr) = tokio::join!(s.publish(event), w.publish(event));
                sr?;
                if let Err(e) = wr {
                    tracing::warn!(error = %e, "webhook event delivery failed");
                }
                Ok(())
            }
            Self::StorageNats(s, n) => {
                let (sr, nr) = tokio::join!(s.publish(event), n.publish(event));
                sr?;
                if let Err(e) = nr {
                    tracing::warn!(error = %e, "NATS event delivery failed");
                }
                Ok(())
            }
            Self::StorageBoth(s, w, n) => {
                let (sr, wr, nr) =
                    tokio::join!(s.publish(event), w.publish(event), n.publish(event));
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
