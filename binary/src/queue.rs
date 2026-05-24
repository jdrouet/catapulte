use catapulte_domain::entity::email::EmailId;
use catapulte_domain::entity::envelope::Envelope;
use catapulte_domain::port::email_queue::{EmailQueue, EmailQueueError};
use catapulte_outbound_queue_memory::MemoryQueue;
use catapulte_outbound_sqlite::SqliteAdapter;

#[derive(Clone)]
pub(crate) enum QueueAdapter {
    Sqlite(SqliteAdapter),
    Memory(MemoryQueue),
}

impl EmailQueue for QueueAdapter {
    async fn enqueue(&self, id: EmailId, envelope: &Envelope) -> Result<(), EmailQueueError> {
        match self {
            Self::Sqlite(a) => a.enqueue(id, envelope).await,
            Self::Memory(q) => q.enqueue(id, envelope).await,
        }
    }

    async fn dequeue(&self) -> Result<(EmailId, Envelope, u32), EmailQueueError> {
        match self {
            Self::Sqlite(a) => a.dequeue().await,
            Self::Memory(q) => q.dequeue().await,
        }
    }

    async fn ack(&self, id: EmailId) -> Result<(), EmailQueueError> {
        match self {
            Self::Sqlite(a) => a.ack(id).await,
            Self::Memory(q) => q.ack(id).await,
        }
    }
}

pub enum QueueBackendConfig {
    Sqlite,
    Memory,
}

impl QueueBackendConfig {
    /// # Errors
    ///
    /// Returns an error if `<prefix>_BACKEND` is set to an unknown value.
    pub fn from_env(prefix: &str) -> anyhow::Result<Self> {
        let key = format!("{prefix}_BACKEND");
        match std::env::var(&key).as_deref() {
            Ok("memory") => Ok(Self::Memory),
            Ok("sqlite") | Err(_) => Ok(Self::Sqlite),
            Ok(other) => anyhow::bail!("unknown queue backend for {key}: {other}"),
        }
    }

    pub(crate) fn build(self, sqlite: &SqliteAdapter) -> QueueAdapter {
        match self {
            Self::Sqlite => QueueAdapter::Sqlite(sqlite.clone()),
            Self::Memory => QueueAdapter::Memory(MemoryQueue::new()),
        }
    }
}
