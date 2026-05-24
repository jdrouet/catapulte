use std::sync::Arc;

use anyhow::Context;
use catapulte_domain::entity::email::EmailId;
use catapulte_domain::entity::envelope::Envelope;
use catapulte_domain::port::email_queue::{EmailQueue, EmailQueueError};
use catapulte_domain::port::event_publisher::EventPublisher;
use catapulte_domain::use_case::process_queued_email::{
    ProcessQueuedEmailService, ProcessQueuedEmailUseCase,
};
use catapulte_domain::use_case::submit_email::{SubmitEmailService, SubmitEmailUseCase};
use catapulte_inbound_http::{HttpServerState, InboundHttpConfig, InboundHttpServer};
use catapulte_inbound_worker::worker::{Worker, WorkerConfig, WorkerState};
use catapulte_outbound_interpolator::interpolator::MiniJinjaInterpolator;
use catapulte_outbound_mjml::renderer::MjmlRenderer;
use catapulte_outbound_queue_memory::MemoryQueue;
use catapulte_outbound_resolver::resolver::{TemplateResolverAdapter, TemplateResolverConfig};
use catapulte_outbound_smtp::sender::{SmtpConfig, SmtpSender};
use catapulte_outbound_sqlite::{SqliteAdapter, SqliteConfig};

type ProcessService = ProcessQueuedEmailService<
    TemplateResolverAdapter,
    MiniJinjaInterpolator,
    MjmlRenderer,
    SmtpSender,
>;

#[derive(Clone)]
enum QueueAdapter {
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

    async fn dequeue(&self) -> Result<(EmailId, Envelope), EmailQueueError> {
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
}

#[derive(Clone)]
struct AppState {
    submit_email: Arc<SubmitEmailService<SqliteAdapter, QueueAdapter>>,
    process_queued_email: Arc<ProcessService>,
    sqlite: SqliteAdapter,
    queue: QueueAdapter,
}

impl HttpServerState for AppState {
    fn submit_email(&self) -> &impl SubmitEmailUseCase {
        self.submit_email.as_ref()
    }
}

impl WorkerState for AppState {
    fn process_queued_email(&self) -> &impl ProcessQueuedEmailUseCase {
        self.process_queued_email.as_ref()
    }

    fn email_queue(&self) -> &impl EmailQueue {
        &self.queue
    }

    fn event_publisher(&self) -> &impl EventPublisher {
        &self.sqlite
    }
}

pub struct AppConfig {
    pub sqlite: SqliteConfig,
    pub http: InboundHttpConfig,
    pub smtp: SmtpConfig,
    pub resolver: TemplateResolverConfig,
    pub worker: WorkerConfig,
    pub queue: QueueBackendConfig,
}

fn load_transport_configs() -> anyhow::Result<(SqliteConfig, InboundHttpConfig, SmtpConfig)> {
    let sqlite = SqliteConfig::from_env("CATAPULTE_SQLITE").context("loading sqlite config")?;
    let http = InboundHttpConfig::from_env("CATAPULTE_HTTP").context("loading http config")?;
    let smtp = SmtpConfig::from_env("CATAPULTE_SMTP").context("loading smtp config")?;
    Ok((sqlite, http, smtp))
}

fn load_processing_configs() -> anyhow::Result<(TemplateResolverConfig, WorkerConfig)> {
    let resolver = TemplateResolverConfig::from_env("CATAPULTE_RESOLVER")
        .context("loading resolver config")?;
    let worker = WorkerConfig::from_env("CATAPULTE_WORKER").context("loading worker config")?;
    Ok((resolver, worker))
}

impl AppConfig {
    /// # Errors
    ///
    /// Returns an error when any sub-config cannot be loaded from environment variables.
    pub fn from_env() -> anyhow::Result<Self> {
        let (sqlite, http, smtp) = load_transport_configs()?;
        let (resolver, worker) = load_processing_configs()?;
        let queue = QueueBackendConfig::from_env("CATAPULTE_QUEUE")
            .context("loading queue backend config")?;
        Ok(Self {
            sqlite,
            http,
            smtp,
            resolver,
            worker,
            queue,
        })
    }

    /// # Errors
    ///
    /// Returns an error when an adapter fails to build.
    pub async fn build(self) -> anyhow::Result<Application> {
        let sqlite = self
            .sqlite
            .build()
            .await
            .context("building sqlite adapter")?;
        sqlite
            .migrate()
            .await
            .context("running sqlite migrations")?;

        let queue = match self.queue {
            QueueBackendConfig::Sqlite => QueueAdapter::Sqlite(sqlite.clone()),
            QueueBackendConfig::Memory => QueueAdapter::Memory(MemoryQueue::new()),
        };

        let smtp = self.smtp.build().context("building smtp sender")?;
        let resolver = self
            .resolver
            .build()
            .context("building template resolver")?;

        let submit_email = Arc::new(SubmitEmailService::new(sqlite.clone(), queue.clone()));
        let process_queued_email = Arc::new(ProcessQueuedEmailService::new(
            resolver,
            MiniJinjaInterpolator::new(),
            MjmlRenderer::new(),
            smtp,
        ));

        let state = AppState {
            submit_email,
            process_queued_email,
            sqlite,
            queue,
        };
        let server = self.http.build();
        let worker = self.worker.build();

        Ok(Application {
            state,
            server,
            worker,
        })
    }
}

pub struct Application {
    state: AppState,
    server: InboundHttpServer,
    worker: Worker,
}

impl Application {
    /// # Errors
    ///
    /// Returns an error when the HTTP server fails to bind or exits unexpectedly.
    pub async fn run(self) -> anyhow::Result<()> {
        tracing::info!("catapulte starting");
        let http = self.server.run(self.state.clone());
        let worker = self.worker.run(self.state);
        tokio::select! {
            result = http => result.context("http server stopped"),
            () = worker => Ok(()),
        }
    }
}
