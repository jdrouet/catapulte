use anyhow::Context;
use catapulte_domain::entity::email::EmailId;
use catapulte_domain::entity::envelope::Envelope;
use catapulte_domain::entity::lifecycle_event::LifecycleEvent;
use catapulte_domain::port::email_repository::{
    EmailRecord, EmailRepository, EmailRepositoryError, ListEmailsParams, SaveResult,
};
use catapulte_domain::port::event_publisher::{EventPublisher, EventPublisherError};
use catapulte_domain::port::event_repository::{
    EventRecord, EventRepository, EventRepositoryError, ListEventsParams,
};
use catapulte_outbound_postgres::{PostgresAdapter, PostgresConfig};
use catapulte_outbound_sqlite::{SqliteAdapter, SqliteConfig};

#[derive(Clone)]
pub enum StorageAdapter {
    Sqlite(SqliteAdapter),
    Postgres(PostgresAdapter),
}

impl EmailRepository for StorageAdapter {
    async fn save(
        &self,
        id: EmailId,
        envelope: &Envelope,
    ) -> Result<SaveResult, EmailRepositoryError> {
        match self {
            Self::Sqlite(a) => a.save(id, envelope).await,
            Self::Postgres(a) => a.save(id, envelope).await,
        }
    }

    async fn list_emails(
        &self,
        params: ListEmailsParams,
    ) -> Result<Vec<EmailRecord>, EmailRepositoryError> {
        match self {
            Self::Sqlite(a) => a.list_emails(params).await,
            Self::Postgres(a) => a.list_emails(params).await,
        }
    }
}

impl EventPublisher for StorageAdapter {
    async fn publish(&self, event: &LifecycleEvent) -> Result<(), EventPublisherError> {
        match self {
            Self::Sqlite(a) => a.publish(event).await,
            Self::Postgres(a) => a.publish(event).await,
        }
    }
}

impl EventRepository for StorageAdapter {
    async fn list_events(
        &self,
        params: ListEventsParams,
    ) -> Result<Vec<EventRecord>, EventRepositoryError> {
        match self {
            Self::Sqlite(a) => a.list_events(params).await,
            Self::Postgres(a) => a.list_events(params).await,
        }
    }
}

impl catapulte_domain::port::sender_usage::SenderUsage for StorageAdapter {
    async fn get_stats(
        &self,
        names: &[catapulte_domain::entity::sender::SenderName],
        since_ms: i64,
    ) -> Result<
        Vec<catapulte_domain::port::sender_usage::SenderStats>,
        catapulte_domain::port::sender_usage::SenderUsageError,
    > {
        match self {
            Self::Sqlite(a) => {
                catapulte_domain::port::sender_usage::SenderUsage::get_stats(a, names, since_ms)
                    .await
            }
            Self::Postgres(a) => {
                catapulte_domain::port::sender_usage::SenderUsage::get_stats(a, names, since_ms)
                    .await
            }
        }
    }
}

pub enum StorageBackendConfig {
    Sqlite(SqliteConfig),
    Postgres(PostgresConfig),
}

impl StorageBackendConfig {
    /// # Errors
    ///
    /// Returns an error if the backend env var is unknown or the sub-config is missing.
    pub fn from_env() -> anyhow::Result<Self> {
        match std::env::var("CATAPULTE_STORAGE_BACKEND")
            .as_deref()
            .unwrap_or("sqlite")
        {
            "postgres" | "pg" => Ok(Self::Postgres(
                PostgresConfig::from_env("CATAPULTE_POSTGRES")
                    .context("loading postgres config")?,
            )),
            _ => Ok(Self::Sqlite(
                SqliteConfig::from_env("CATAPULTE_SQLITE").context("loading sqlite config")?,
            )),
        }
    }

    /// # Errors
    ///
    /// Returns an error if the adapter fails to connect or migrate.
    pub async fn build(self) -> anyhow::Result<StorageAdapter> {
        match self {
            Self::Sqlite(cfg) => {
                let adapter = cfg.build().await.context("building sqlite adapter")?;
                adapter
                    .migrate()
                    .await
                    .context("running sqlite migrations")?;
                Ok(StorageAdapter::Sqlite(adapter))
            }
            Self::Postgres(cfg) => {
                let adapter = cfg.build().await.context("building postgres adapter")?;
                adapter
                    .migrate()
                    .await
                    .context("running postgres migrations")?;
                Ok(StorageAdapter::Postgres(adapter))
            }
        }
    }
}
