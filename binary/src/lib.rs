use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use catapulte_domain::use_case::process_queued_email::ProcessQueuedEmailService;
use catapulte_domain::use_case::submit_email::SubmitEmailService;
use catapulte_inbound_http::{InboundHttpConfig, InboundHttpServer};
use catapulte_inbound_nats::server::{InboundNatsConfig, InboundNatsServer};
use catapulte_inbound_worker::worker::{Worker, WorkerConfig};
use catapulte_outbound_attachment_fetcher::fetcher::HttpAttachmentFetcher;
use catapulte_outbound_attachment_fs::store::FsAttachmentStore;
use catapulte_outbound_interpolator::interpolator::MiniJinjaInterpolator;
use catapulte_outbound_mjml::renderer::MjmlRenderer;
use catapulte_outbound_resolver::resolver::TemplateResolverConfig;
use catapulte_outbound_smtp::multi_sender::MultiSenderConfig;

pub mod attachment_fetcher;
pub mod attachment_store;
pub mod gc;
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
    pub inbound_nats: Option<InboundNatsConfig>,
    pub smtp: MultiSenderConfig,
    pub resolver: TemplateResolverConfig,
    pub worker: WorkerConfig,
    pub queue: queue::QueueBackendConfig,
    pub publisher: PublisherAdapterConfig,
    pub attachment_store: attachment_store::AttachmentStoreBackendConfig,
    pub attachment_fetcher:
        catapulte_outbound_attachment_fetcher::fetcher::HttpAttachmentFetcherConfig,
    pub include_loader: catapulte_outbound_mjml::include_loader::IncludeLoaderConfig,
    pub gc_sweep_interval: Duration,
    pub gc_grace_period: Duration,
}

impl AppConfig {
    /// # Errors
    ///
    /// Returns an error when any sub-config cannot be loaded from environment variables.
    pub fn from_env() -> anyhow::Result<Self> {
        let storage = StorageBackendConfig::from_env().context("loading storage config")?;
        let http = InboundHttpConfig::from_env("CATAPULTE_HTTP").context("loading http config")?;
        let inbound_nats = InboundNatsConfig::from_env("CATAPULTE_INBOUND_NATS")
            .context("loading inbound NATS config")?;
        let smtp = MultiSenderConfig::from_env().context("loading smtp config")?;
        let resolver = TemplateResolverConfig::from_env("CATAPULTE_RESOLVER")
            .context("loading resolver config")?;
        let worker = WorkerConfig::from_env("CATAPULTE_WORKER").context("loading worker config")?;
        let queue = queue::QueueBackendConfig::from_env("CATAPULTE_QUEUE")
            .context("loading queue backend config")?;
        let publisher = PublisherAdapterConfig::from_env().context("loading publisher config")?;
        let attachment_store = attachment_store::AttachmentStoreBackendConfig::from_env()
            .context("loading attachment store config")?;
        let attachment_fetcher =
            catapulte_outbound_attachment_fetcher::fetcher::HttpAttachmentFetcherConfig::from_env(
                "CATAPULTE_ATTACHMENT_FETCHER",
            )
            .context("loading attachment fetcher config")?;
        let include_loader =
            catapulte_outbound_mjml::include_loader::IncludeLoaderConfig::from_env(
                "CATAPULTE_INCLUDE_LOADER",
            )
            .context("loading include loader config")?;
        let gc_sweep_secs: u64 = std::env::var("CATAPULTE_GC_SWEEP_INTERVAL_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(3600);
        let gc_sweep_interval = Duration::from_secs(gc_sweep_secs);
        let gc_grace_secs: u64 = std::env::var("CATAPULTE_GC_GRACE_PERIOD_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(3600);
        let gc_grace_period = Duration::from_secs(gc_grace_secs);
        Ok(Self {
            storage,
            http,
            inbound_nats,
            smtp,
            resolver,
            worker,
            queue,
            publisher,
            attachment_store,
            attachment_fetcher,
            include_loader,
            gc_sweep_interval,
            gc_grace_period,
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
                catapulte_outbound_smtp::transport::SmtpTransport,
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
        let list_emails = Arc::new(
            catapulte_domain::use_case::list_emails::ListEmailsService::new(storage.clone()),
        );
        let list_events = Arc::new(
            catapulte_domain::use_case::list_events::ListEventsService::new(storage.clone()),
        );
        let resolver = self
            .resolver
            .build()
            .context("building template resolver")?;

        let attachment_store = self
            .attachment_store
            .build()
            .await
            .context("building attachment store adapter")?;

        let attachment_fetcher: HttpAttachmentFetcher = self
            .attachment_fetcher
            .build()
            .context("building attachment fetcher adapter")?;

        let submit_email = Arc::new(SubmitEmailService::new(
            storage.clone(),
            queue.clone(),
            publisher.clone(),
            attachment_store.clone(),
            attachment_fetcher,
        ));
        let mjml_renderer = MjmlRenderer::new(self.include_loader.build());
        let process_queued_email = Arc::new(ProcessQueuedEmailService::new(
            resolver,
            MiniJinjaInterpolator::new(),
            mjml_renderer,
            smtp,
            attachment_store.clone(),
        ));

        let state = AppState {
            submit_email,
            process_queued_email,
            list_senders,
            list_emails,
            list_events,
            queue,
            publisher,
            attachment_store: attachment_store.clone(),
            storage: storage.clone(),
        };
        let server = self.http.build();
        let worker = self.worker.build();

        let gc_fs_store: Option<FsAttachmentStore> = attachment_store.as_fs().map(Clone::clone);
        let gc = gc_fs_store.map(|fs_store| {
            gc::AttachmentGc::new(
                storage.clone(),
                fs_store,
                self.gc_sweep_interval,
                self.gc_grace_period,
            )
        });

        let inbound_nats_server = match self.inbound_nats {
            Some(cfg) => Some(cfg.build().await.context("building inbound NATS server")?),
            None => None,
        };

        Ok(Application {
            state,
            server,
            worker,
            gc,
            inbound_nats_server,
        })
    }
}

pub struct Application {
    state: AppState,
    server: InboundHttpServer,
    inbound_nats_server: Option<InboundNatsServer>,
    worker: Worker,
    gc: Option<gc::AttachmentGc>,
}

impl Application {
    /// # Errors
    ///
    /// Returns an error when the HTTP server fails to bind or exits unexpectedly.
    pub async fn run(self) -> anyhow::Result<()> {
        tracing::info!("catapulte starting");
        let cancel = tokio_util::sync::CancellationToken::new();
        let http = self.server.run(self.state.clone());

        let inbound_nats_handle = self.inbound_nats_server.map(|s| {
            let s_state = self.state.clone();
            let s_cancel = cancel.clone();
            tokio::spawn(async move { s.run(s_state, s_cancel).await })
        });

        let worker = self.worker.run(self.state, cancel.clone());

        let gc_handle = self.gc.map(|gc| {
            let gc_cancel = cancel.clone();
            tokio::spawn(async move { gc.run(gc_cancel).await })
        });

        match (gc_handle, inbound_nats_handle) {
            (Some(gc_handle), Some(inbound_nats_handle)) => {
                tokio::select! {
                    result = http => {
                        cancel.cancel();
                        result.context("http server stopped")
                    }
                    () = worker => Ok(()),
                    result = gc_handle => {
                        if let Err(e) = result {
                            tracing::error!(error = %e, "attachment GC task ended unexpectedly");
                        } else {
                            tracing::info!("attachment GC task ended");
                        }
                        cancel.cancel();
                        Ok(())
                    }
                    result = inbound_nats_handle => {
                        if let Err(e) = result {
                            tracing::error!(error = %e, "inbound NATS task ended unexpectedly");
                        } else {
                            tracing::info!("inbound NATS task ended");
                        }
                        cancel.cancel();
                        Ok(())
                    }
                    result = tokio::signal::ctrl_c() => {
                        result.context("failed to listen for shutdown signal")?;
                        tracing::info!("shutdown signal received");
                        cancel.cancel();
                        Ok(())
                    }
                }
            }
            (Some(gc_handle), None) => {
                tokio::select! {
                    result = http => {
                        cancel.cancel();
                        result.context("http server stopped")
                    }
                    () = worker => Ok(()),
                    result = gc_handle => {
                        if let Err(e) = result {
                            tracing::error!(error = %e, "attachment GC task ended unexpectedly");
                        } else {
                            tracing::info!("attachment GC task ended");
                        }
                        cancel.cancel();
                        Ok(())
                    }
                    result = tokio::signal::ctrl_c() => {
                        result.context("failed to listen for shutdown signal")?;
                        tracing::info!("shutdown signal received");
                        cancel.cancel();
                        Ok(())
                    }
                }
            }
            (None, Some(inbound_nats_handle)) => {
                tokio::select! {
                    result = http => {
                        cancel.cancel();
                        result.context("http server stopped")
                    }
                    () = worker => Ok(()),
                    result = inbound_nats_handle => {
                        if let Err(e) = result {
                            tracing::error!(error = %e, "inbound NATS task ended unexpectedly");
                        } else {
                            tracing::info!("inbound NATS task ended");
                        }
                        cancel.cancel();
                        Ok(())
                    }
                    result = tokio::signal::ctrl_c() => {
                        result.context("failed to listen for shutdown signal")?;
                        tracing::info!("shutdown signal received");
                        cancel.cancel();
                        Ok(())
                    }
                }
            }
            (None, None) => {
                tokio::select! {
                    result = http => {
                        cancel.cancel();
                        result.context("http server stopped")
                    }
                    () = worker => Ok(()),
                    result = tokio::signal::ctrl_c() => {
                        result.context("failed to listen for shutdown signal")?;
                        tracing::info!("shutdown signal received");
                        cancel.cancel();
                        Ok(())
                    }
                }
            }
        }
    }
}
