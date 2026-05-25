use std::sync::Arc;

use anyhow::Context;
use catapulte_domain::use_case::process_queued_email::ProcessQueuedEmailService;
use catapulte_domain::use_case::submit_email::SubmitEmailService;
use catapulte_inbound_http::{InboundHttpConfig, InboundHttpServer};
use catapulte_inbound_worker::worker::{Worker, WorkerConfig};
use catapulte_outbound_interpolator::interpolator::MiniJinjaInterpolator;
use catapulte_outbound_mjml::renderer::MjmlRenderer;
use catapulte_outbound_resolver::resolver::TemplateResolverConfig;
use catapulte_outbound_smtp::multi_sender::MultiSenderConfig;

pub mod publisher;
pub mod queue;
mod state;
pub mod storage;

use publisher::PublisherAdapterConfig;
use state::AppState;
use storage::StorageBackendConfig;

pub struct AppConfig {
    pub storage: StorageBackendConfig,
    pub http: InboundHttpConfig,
    pub smtp: MultiSenderConfig,
    pub resolver: TemplateResolverConfig,
    pub worker: WorkerConfig,
    pub queue: queue::QueueBackendConfig,
    pub publisher: PublisherAdapterConfig,
}

impl AppConfig {
    /// # Errors
    ///
    /// Returns an error when any sub-config cannot be loaded from environment variables.
    pub fn from_env() -> anyhow::Result<Self> {
        let storage = StorageBackendConfig::from_env().context("loading storage config")?;
        let http = InboundHttpConfig::from_env("CATAPULTE_HTTP").context("loading http config")?;
        let smtp = MultiSenderConfig::from_env().context("loading smtp config")?;
        let resolver = TemplateResolverConfig::from_env("CATAPULTE_RESOLVER")
            .context("loading resolver config")?;
        let worker = WorkerConfig::from_env("CATAPULTE_WORKER").context("loading worker config")?;
        let queue = queue::QueueBackendConfig::from_env("CATAPULTE_QUEUE")
            .context("loading queue backend config")?;
        let publisher = PublisherAdapterConfig::from_env().context("loading publisher config")?;
        Ok(Self {
            storage,
            http,
            smtp,
            resolver,
            worker,
            queue,
            publisher,
        })
    }

    /// # Errors
    ///
    /// Returns an error when an adapter fails to build.
    pub async fn build(self) -> anyhow::Result<Application> {
        let storage = self
            .storage
            .build()
            .await
            .context("building storage adapter")?;

        let queue = self
            .queue
            .build(&storage)
            .await
            .context("building queue adapter")?;

        let publisher = self
            .publisher
            .build(storage.clone())
            .await
            .context("building publisher adapter")?;

        let entries = self.smtp.build().context("building smtp transports")?;
        let sender_configs: Vec<catapulte_domain::entity::sender::SenderConfig> = entries
            .iter()
            .map(|e| catapulte_domain::entity::sender::SenderConfig {
                name: e.name.clone(),
                quota: e.quota.clone(),
            })
            .collect();
        let routes: Vec<
            catapulte_domain::service::routed_email_sender::SenderRoute<
                catapulte_outbound_smtp::sender::SmtpSender,
            >,
        > = entries
            .into_iter()
            .map(
                |e| catapulte_domain::service::routed_email_sender::SenderRoute {
                    name: e.name,
                    priority: e.priority,
                    quota: e.quota,
                    transport: e.transport,
                },
            )
            .collect();
        let smtp = catapulte_domain::service::routed_email_sender::RoutedEmailSender::new(
            routes,
            storage.clone(),
            catapulte_domain::port::clock::SystemClock,
        )
        .context("building routed email sender")?;
        let list_senders = Arc::new(
            catapulte_domain::use_case::list_senders::ListSendersService::new(
                sender_configs,
                storage.clone(),
                catapulte_domain::port::clock::SystemClock,
            ),
        );
        let resolver = self
            .resolver
            .build()
            .context("building template resolver")?;

        let submit_email = Arc::new(SubmitEmailService::new(
            storage.clone(),
            queue.clone(),
            publisher.clone(),
        ));
        let process_queued_email = Arc::new(ProcessQueuedEmailService::new(
            resolver,
            MiniJinjaInterpolator::new(),
            MjmlRenderer::new(),
            smtp,
        ));

        let state = AppState {
            submit_email,
            process_queued_email,
            list_senders,
            storage,
            queue,
            publisher,
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
