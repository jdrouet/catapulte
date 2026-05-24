use std::time::Duration;

use catapulte_domain::entity::email::EmailId;
use catapulte_domain::entity::envelope::Envelope;
use catapulte_domain::port::email_queue::{AckToken, EmailQueue, EmailQueueError};
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

impl EmailQueue for QueueAdapter {
    async fn enqueue(&self, id: EmailId, envelope: &Envelope) -> Result<(), EmailQueueError> {
        match self {
            Self::Sqlite(a) => a.enqueue(id, envelope).await,
            Self::Postgres(a) => a.enqueue(id, envelope).await,
            Self::Memory(q) => q.enqueue(id, envelope).await,
            Self::Nats(a) => a.enqueue(id, envelope).await,
        }
    }

    async fn dequeue(&self) -> Result<(EmailId, Envelope, u32, AckToken), EmailQueueError> {
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

    async fn nack(&self, token: AckToken, delay: Duration) -> Result<(), EmailQueueError> {
        match self {
            Self::Sqlite(a) => a.nack(token, delay).await,
            Self::Postgres(a) => a.nack(token, delay).await,
            Self::Memory(q) => q.nack(token, delay).await,
            Self::Nats(a) => a.nack(token, delay).await,
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
            _ => Ok(Self::Storage),
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
            Self::Memory => Ok(QueueAdapter::Memory(MemoryQueue::new())),
            Self::Nats(config) => {
                let adapter = config.build().await?;
                Ok(QueueAdapter::Nats(adapter))
            }
        }
    }
}
