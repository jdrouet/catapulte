use std::time::Duration;

use catapulte_domain::entity::email::EmailId;
use catapulte_domain::entity::envelope::Envelope;
use catapulte_domain::entity::lifecycle_event::LifecycleEvent;
use catapulte_domain::port::email_queue::{
    AckToken, DequeuedEmail, EmailQueue, EmailQueueError, EnqueueOutcome, FinishOutcome,
};
use catapulte_domain::port::email_repository::{EmailRepository, EmailStatus, ListEmailsParams};
use catapulte_domain::port::event_publisher::EventPublisher;
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
    /// The storage holds the terminal events, since a `JetStream` ack cannot join an SQL commit.
    Nats(NatsAdapter, StorageAdapter),
}

impl QueueAdapter {
    pub(crate) fn backend_name(&self) -> &'static str {
        match self {
            Self::Sqlite(_) => "sqlite",
            Self::Postgres(_) => "postgres",
            Self::Memory(_) => "memory",
            Self::Nats(..) => "nats",
        }
    }

    /// The furthest ahead an email may be scheduled on this queue, or `None` when the queue
    /// cannot hold scheduled emails. `configured` is lowered to any limit the queue imposes.
    pub(crate) fn schedule_horizon(&self, configured: Duration) -> Option<Duration> {
        match self {
            Self::Sqlite(_) | Self::Postgres(_) | Self::Memory(_) => Some(configured),
            Self::Nats(a, _) if a.supports_scheduling() => {
                Some(a.schedule_limit().map_or(configured, |l| configured.min(l)))
            }
            Self::Nats(..) => None,
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
            Self::Nats(a, _) => match a.pending().await {
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
            Self::Nats(a, _) => a.enqueue(id, envelope).await,
        }
    }

    async fn dequeue(&self) -> Result<DequeuedEmail, EmailQueueError> {
        match self {
            Self::Sqlite(a) => a.dequeue().await,
            Self::Postgres(a) => a.dequeue().await,
            Self::Memory(q) => q.dequeue().await,
            Self::Nats(a, storage) => loop {
                let dequeued = a.dequeue().await?;
                if !is_finished(storage, dequeued.id).await? {
                    return Ok(dequeued);
                }
                // Redelivered after its terminal event was stored but before the ack.
                tracing::info!(email_id = %dequeued.id.as_uuid(), "skipping an already finished email");
                if let Err(e) = a.ack(dequeued.token).await {
                    tracing::warn!(error = %e, "failed to ack an already finished email");
                }
            },
        }
    }

    async fn ack(&self, token: AckToken) -> Result<(), EmailQueueError> {
        match self {
            Self::Sqlite(a) => a.ack(token).await,
            Self::Postgres(a) => a.ack(token).await,
            Self::Memory(q) => q.ack(token).await,
            Self::Nats(a, _) => a.ack(token).await,
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
            // Event first: a crash before the ack redelivers the email, and dequeue skips it.
            Self::Nats(a, storage) => {
                storage
                    .publish(event)
                    .await
                    .map_err(|e| EmailQueueError::Storage { source: e.into() })?;
                a.ack(token).await.map(|()| FinishOutcome::Recorded)
            }
        }
    }

    async fn nack(&self, token: AckToken, delay: Duration) -> Result<(), EmailQueueError> {
        match self {
            Self::Sqlite(a) => a.nack(token, delay).await,
            Self::Postgres(a) => a.nack(token, delay).await,
            Self::Memory(q) => q.nack(token, delay).await,
            Self::Nats(a, _) => a.nack(token, delay).await,
        }
    }

    async fn in_progress(&self, token: &AckToken) -> Result<(), EmailQueueError> {
        match self {
            Self::Sqlite(a) => a.in_progress(token).await,
            Self::Postgres(a) => a.in_progress(token).await,
            Self::Memory(q) => q.in_progress(token).await,
            Self::Nats(a, _) => a.in_progress(token).await,
        }
    }

    fn renew_interval(&self) -> Duration {
        match self {
            Self::Sqlite(a) => a.renew_interval(),
            Self::Postgres(a) => a.renew_interval(),
            Self::Memory(q) => q.renew_interval(),
            Self::Nats(a, _) => a.renew_interval(),
        }
    }
}

async fn is_finished(storage: &StorageAdapter, id: EmailId) -> Result<bool, EmailQueueError> {
    let emails = storage
        .list_emails(ListEmailsParams {
            tag: None,
            owner: None,
            status: None,
            after_ms: None,
            before_ms: None,
            recipient: None,
            template: None,
            id: Some(id),
            limit: 1,
            offset: 0,
        })
        .await
        .map_err(|e| EmailQueueError::Storage { source: e.into() })?;
    Ok(emails
        .iter()
        .any(|e| matches!(e.status, EmailStatus::Sent | EmailStatus::Failed)))
}

impl catapulte_domain::port::health::HealthCheck for QueueAdapter {
    async fn check(&self) -> Result<(), catapulte_domain::port::health::HealthCheckError> {
        match self {
            Self::Sqlite(_) | Self::Postgres(_) | Self::Memory(_) => Ok(()),
            Self::Nats(a, _) => catapulte_domain::port::health::HealthCheck::check(a).await,
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
                Ok(QueueAdapter::Nats(adapter, storage.clone()))
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
    use catapulte_domain::port::event_publisher::EventPublisher;
    use catapulte_domain::port::event_repository::{EventRepository, ListEventsParams};
    use catapulte_outbound_nats::NatsConfig;
    use catapulte_outbound_sqlite::SqliteAdapter;
    use std::time::Duration;
    use testcontainers::core::{IntoContainerPort, WaitFor};
    use testcontainers::runners::AsyncRunner;
    use testcontainers::{ContainerAsync, GenericImage, ImageExt};

    use super::QueueAdapter;
    use crate::storage::StorageAdapter;

    fn sent(id: EmailId) -> LifecycleEvent {
        LifecycleEvent::Sent {
            id,
            sender_name: SenderName::new("primary"),
            context: EventContext::with_correlation(None),
            smtp_response: None,
            message_id: "<test@example.com>".to_owned(),
        }
    }

    struct NatsHarness {
        queue: QueueAdapter,
        sqlite: SqliteAdapter,
        url: String,
        _nats: ContainerAsync<GenericImage>,
    }

    impl NatsHarness {
        async fn start() -> Self {
            let nats = GenericImage::new("nats", "2-alpine")
                .with_exposed_port(4222.tcp())
                .with_wait_for(WaitFor::message_on_stderr("Server is ready"))
                .with_wait_for(WaitFor::millis(200))
                .with_cmd(["nats-server".to_owned(), "--js".to_owned()])
                .start()
                .await
                .expect("failed to start NATS container; ensure Docker is running");
            let url = format!(
                "nats://127.0.0.1:{}",
                nats.get_host_port_ipv4(4222).await.unwrap()
            );
            let adapter = NatsConfig {
                url: url.clone(),
                stream: "FINISH".to_owned(),
                subject: "finish.queued".to_owned(),
                consumer: "worker".to_owned(),
                ack_wait_secs: 1,
                max_deliver: 3,
                backoff_secs: vec![1, 1],
            }
            .build()
            .await
            .unwrap();
            let sqlite = SqliteAdapter::connect(":memory:").await.unwrap();
            sqlite.migrate().await.unwrap();
            Self {
                queue: QueueAdapter::Nats(adapter, StorageAdapter::Sqlite(sqlite.clone())),
                sqlite,
                url,
                _nats: nats,
            }
        }

        async fn enqueue(&self) -> EmailId {
            let id = EmailId::default();
            let envelope = Envelope::new(
                "sender@example.com",
                vec![],
                BodySource::Plain(Plain::try_new(Some("hello".to_owned()), None).unwrap()),
            );
            self.sqlite.save(id, &envelope).await.unwrap();
            self.queue.enqueue(id, &envelope).await.unwrap();
            id
        }

        async fn unacked(&self) -> usize {
            let client = async_nats::connect(&self.url).await.unwrap();
            let stream = async_nats::jetstream::new(client)
                .get_stream("FINISH")
                .await
                .unwrap();
            let mut consumer = stream
                .get_consumer::<async_nats::jetstream::consumer::pull::Config>("worker")
                .await
                .unwrap();
            consumer.info().await.unwrap().num_ack_pending
        }

        async fn succeeded_events(&self, id: EmailId) -> usize {
            self.sqlite
                .list_events(ListEventsParams {
                    owner: None,
                    email_id: Some(id),
                    event_type: Some("delivery.succeeded".to_owned()),
                    sender_name: None,
                    error_class: None,
                    failure_category: None,
                    after_ms: None,
                    before_ms: None,
                    limit: 10,
                    offset: 0,
                })
                .await
                .unwrap()
                .len()
        }
    }

    #[tokio::test]
    async fn nats_queue_finish_records_the_event_and_acks() {
        let h = NatsHarness::start().await;
        let id = h.enqueue().await;
        let dequeued = h.queue.dequeue().await.unwrap();

        let outcome = h.queue.finish(dequeued.token, &sent(id)).await.unwrap();

        assert_eq!(outcome, FinishOutcome::Recorded);
        assert_eq!(h.succeeded_events(id).await, 1);
        assert_eq!(h.unacked().await, 0);
    }

    #[tokio::test]
    async fn nats_queue_skips_a_redelivered_email_already_finished() {
        let h = NatsHarness::start().await;
        let id = h.enqueue().await;
        let dequeued = h.queue.dequeue().await.unwrap();
        // The process dies after storing the terminal event and before the ack.
        h.sqlite.publish(&sent(id)).await.unwrap();
        drop(dequeued);

        let next = tokio::time::timeout(Duration::from_secs(4), h.queue.dequeue()).await;

        assert!(next.is_err(), "the finished email was handed out again");
        assert_eq!(h.unacked().await, 0);
        assert_eq!(h.succeeded_events(id).await, 1);
    }

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
