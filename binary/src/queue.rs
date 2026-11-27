use std::time::Duration;

use catapulte_domain::entity::email::EmailId;
use catapulte_domain::entity::envelope::Envelope;
use catapulte_domain::entity::lifecycle_event::LifecycleEvent;
use catapulte_domain::port::email_queue::{
    AckToken, DequeuedEmail, EmailQueue, EmailQueueError, EnqueueOutcome, FinishOutcome,
};
use catapulte_outbound_nats::{NatsAdapter, NatsConfig};
use catapulte_outbound_postgres::PostgresAdapter;
use catapulte_outbound_queue_memory::MemoryQueue;
use catapulte_outbound_sqlite::SqliteAdapter;

use crate::storage::StorageAdapter;

#[derive(Clone)]
pub(crate) enum QueueAdapter {
    Sqlite(SqliteAdapter),
    Postgres(PostgresAdapter),
    Memory(MemoryQueue),
    Nats(NatsAdapter),
}

impl QueueAdapter {
    pub(crate) fn backend_name(&self) -> &'static str {
        match self {
            Self::Sqlite(_) => "sqlite",
            Self::Postgres(_) => "postgres",
            Self::Memory(_) => "memory",
            Self::Nats(_) => "nats",
        }
    }

    /// The furthest ahead an email may be scheduled on this queue, or `None` when the queue
    /// cannot hold scheduled emails. `configured` is lowered to any limit the queue imposes.
    pub(crate) fn schedule_horizon(&self, configured: Duration) -> Option<Duration> {
        match self {
            Self::Sqlite(_) | Self::Postgres(_) | Self::Memory(_) => Some(configured),
            Self::Nats(a) if a.supports_scheduling() => {
                Some(a.schedule_limit().map_or(configured, |l| configured.min(l)))
            }
            Self::Nats(_) => None,
        }
    }

    /// Returns the number of pending queue entries, or `None` on error.
    pub(crate) async fn pending(&self) -> Option<u64> {
        match self {
            Self::Sqlite(a) => match a.pending().await {
                Ok(n) => Some(n),
                Err(e) => {
                    tracing::warn!(error = %e, "failed to read pending queue count");
                    None
                }
            },
            Self::Postgres(a) => match a.pending().await {
                Ok(n) => Some(n),
                Err(e) => {
                    tracing::warn!(error = %e, "failed to read pending queue count");
                    None
                }
            },
            Self::Memory(q) => Some(q.pending()),
            Self::Nats(a) => match a.pending().await {
                Ok(n) => Some(n),
                Err(e) => {
                    tracing::warn!(error = %e, "failed to read pending queue count");
                    None
                }
            },
        }
    }
}

impl EmailQueue for QueueAdapter {
    #[tracing::instrument(skip_all, name = "queue.enqueue", fields(backend = self.backend_name()))]
    async fn enqueue(
        &self,
        id: EmailId,
        envelope: &Envelope,
    ) -> Result<EnqueueOutcome, EmailQueueError> {
        match self {
            Self::Sqlite(a) => a.enqueue(id, envelope).await,
            Self::Postgres(a) => a.enqueue(id, envelope).await,
            Self::Memory(q) => q.enqueue(id, envelope).await,
            Self::Nats(a) => a.enqueue(id, envelope).await,
        }
    }

    async fn dequeue(&self) -> Result<DequeuedEmail, EmailQueueError> {
        match self {
            Self::Sqlite(a) => a.dequeue().await,
            Self::Postgres(a) => a.dequeue().await,
            Self::Memory(q) => q.dequeue().await,
            Self::Nats(a) => a.dequeue().await,
        }
    }

    async fn ack(&self, token: AckToken) -> Result<(), EmailQueueError> {
        match self {
            Self::Sqlite(a) => a.ack(token).await,
            Self::Postgres(a) => a.ack(token).await,
            Self::Memory(q) => q.ack(token).await,
            Self::Nats(a) => a.ack(token).await,
        }
    }

    async fn finish(
        &self,
        token: AckToken,
        event: &LifecycleEvent,
    ) -> Result<FinishOutcome, EmailQueueError> {
        match self {
            Self::Sqlite(a) => a.finish(token, event).await,
            Self::Postgres(a) => a.finish(token, event).await,
            Self::Memory(q) => q.finish(token, event).await,
            Self::Nats(a) => a.finish(token, event).await,
        }
    }

    async fn nack(&self, token: AckToken, delay: Duration) -> Result<(), EmailQueueError> {
        match self {
            Self::Sqlite(a) => a.nack(token, delay).await,
            Self::Postgres(a) => a.nack(token, delay).await,
            Self::Memory(q) => q.nack(token, delay).await,
            Self::Nats(a) => a.nack(token, delay).await,
        }
    }

    async fn in_progress(&self, token: &AckToken) -> Result<(), EmailQueueError> {
        match self {
            Self::Sqlite(a) => a.in_progress(token).await,
            Self::Postgres(a) => a.in_progress(token).await,
            Self::Memory(q) => q.in_progress(token).await,
            Self::Nats(a) => a.in_progress(token).await,
        }
    }

    fn renew_interval(&self) -> Duration {
        match self {
            Self::Sqlite(a) => a.renew_interval(),
            Self::Postgres(a) => a.renew_interval(),
            Self::Memory(q) => q.renew_interval(),
            Self::Nats(a) => a.renew_interval(),
        }
    }
}

impl catapulte_domain::port::health::HealthCheck for QueueAdapter {
    async fn check(&self) -> Result<(), catapulte_domain::port::health::HealthCheckError> {
        match self {
            Self::Sqlite(_) | Self::Postgres(_) | Self::Memory(_) => Ok(()),
            Self::Nats(a) => catapulte_domain::port::health::HealthCheck::check(a).await,
        }
    }
}

pub enum QueueBackendConfig {
    Storage,
    Memory,
    Nats(NatsConfig),
}

impl QueueBackendConfig {
    /// # Errors
    ///
    /// Returns an error if `<prefix>_BACKEND` is set to an unknown value or if the
    /// NATS config cannot be loaded from environment variables.
    pub fn from_env(prefix: &str) -> anyhow::Result<Self> {
        let key = format!("{prefix}_BACKEND");
        match std::env::var(&key).as_deref() {
            Ok("memory") => Ok(Self::Memory),
            Ok("nats") => Ok(Self::Nats(NatsConfig::from_env(prefix)?)),
            Ok("storage") => Ok(Self::Storage),
            Err(_) => {
                tracing::debug!(key = %key, "env var not set, defaulting to storage queue backend");
                Ok(Self::Storage)
            }
            Ok(unknown) => anyhow::bail!("unknown queue backend {unknown:?} in env var {key}"),
        }
    }

    /// # Errors
    ///
    /// Returns an error if building the NATS adapter fails.
    pub(crate) async fn build(self, storage: &StorageAdapter) -> anyhow::Result<QueueAdapter> {
        match self {
            Self::Storage => Ok(match storage {
                StorageAdapter::Sqlite(a) => QueueAdapter::Sqlite(a.clone()),
                StorageAdapter::Postgres(a) => QueueAdapter::Postgres(a.clone()),
            }),
            Self::Memory => {
                tracing::warn!("using in-memory queue: all queued emails will be lost on restart");
                Ok(QueueAdapter::Memory(MemoryQueue::new()))
            }
            Self::Nats(config) => {
                let adapter = config.build().await?;
                Ok(QueueAdapter::Nats(adapter))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use catapulte_domain::entity::body::{BodySource, Plain};
    use catapulte_domain::entity::email::EmailId;
    use catapulte_domain::entity::envelope::Envelope;
    use catapulte_domain::entity::lifecycle_event::{EventContext, LifecycleEvent};
    use catapulte_domain::entity::sender::SenderName;
    use catapulte_domain::port::email_queue::{EmailQueue, FinishOutcome};
    use catapulte_domain::port::email_repository::EmailRepository;
    use catapulte_outbound_sqlite::SqliteAdapter;

    use super::QueueAdapter;

    #[tokio::test]
    async fn storage_queue_finish_records_the_event() {
        let sqlite = SqliteAdapter::connect(":memory:").await.unwrap();
        sqlite.migrate().await.unwrap();
        let queue = QueueAdapter::Sqlite(sqlite.clone());
        let id = EmailId::default();
        let envelope = Envelope::new(
            "sender@example.com",
            vec![],
            BodySource::Plain(Plain::try_new(Some("hello".to_owned()), None).unwrap()),
        );
        sqlite.save(id, &envelope).await.unwrap();
        queue.enqueue(id, &envelope).await.unwrap();
        let dequeued = queue.dequeue().await.unwrap();

        let outcome = queue
            .finish(
                dequeued.token,
                &LifecycleEvent::Sent {
                    id,
                    sender_name: SenderName::new("primary"),
                    context: EventContext::with_correlation(None),
                    smtp_response: None,
                    message_id: "<test@example.com>".to_owned(),
                },
            )
            .await
            .unwrap();

        assert_eq!(outcome, FinishOutcome::Recorded);
    }
}
