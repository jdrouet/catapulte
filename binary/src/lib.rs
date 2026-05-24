use std::sync::Arc;

use anyhow::Context;
use catapulte_domain::use_case::process_queued_email::ProcessQueuedEmailService;
use catapulte_domain::use_case::submit_email::SubmitEmailService;
use catapulte_inbound_http::{InboundHttpConfig, InboundHttpServer};
use catapulte_inbound_worker::worker::{Worker, WorkerConfig};
use catapulte_outbound_interpolator::interpolator::MiniJinjaInterpolator;
use catapulte_outbound_mjml::renderer::MjmlRenderer;
use catapulte_outbound_resolver::resolver::TemplateResolverConfig;
use catapulte_outbound_smtp::sender::SmtpConfig;
use catapulte_outbound_sqlite::SqliteConfig;

pub mod queue;
mod state;

use state::AppState;

pub struct AppConfig {
    pub sqlite: SqliteConfig,
    pub http: InboundHttpConfig,
    pub smtp: SmtpConfig,
    pub resolver: TemplateResolverConfig,
    pub worker: WorkerConfig,
    pub queue: queue::QueueBackendConfig,
}

impl AppConfig {
    /// # Errors
    ///
    /// Returns an error when any sub-config cannot be loaded from environment variables.
    pub fn from_env() -> anyhow::Result<Self> {
        let sqlite = SqliteConfig::from_env("CATAPULTE_SQLITE").context("loading sqlite config")?;
        let http = InboundHttpConfig::from_env("CATAPULTE_HTTP").context("loading http config")?;
        let smtp = SmtpConfig::from_env("CATAPULTE_SMTP").context("loading smtp config")?;
        let resolver = TemplateResolverConfig::from_env("CATAPULTE_RESOLVER")
            .context("loading resolver config")?;
        let worker = WorkerConfig::from_env("CATAPULTE_WORKER").context("loading worker config")?;
        let queue = queue::QueueBackendConfig::from_env("CATAPULTE_QUEUE")
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

        let queue = self.queue.build(&sqlite);

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
