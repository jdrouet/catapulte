use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use catapulte_domain::use_case::process_queued_email::ProcessQueuedEmailService;
use catapulte_domain::use_case::submit_email::{
    SubmitEmailService, SubmitLimits, normalize_sender_domains,
};
use catapulte_inbound_http::{InboundHttpConfig, InboundHttpServer};
use catapulte_inbound_nats::server::{InboundNatsConfig, InboundNatsServer};
use catapulte_inbound_worker::worker::{Worker, WorkerConfig};
use catapulte_outbound_attachment_fetcher::fetcher::HttpAttachmentFetcher;
use catapulte_outbound_interpolator::interpolator::MiniJinjaInterpolator;
use catapulte_outbound_mjml::renderer::MjmlRenderer;
use catapulte_outbound_resolver::resolver::TemplateResolverConfig;
use catapulte_outbound_smtp::multi_sender::MultiSenderConfig;

pub mod attachment_fetcher;
pub mod attachment_store;
pub mod gc;
mod health;
mod metrics;
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
    /// Single limit shared by the submit-time estimate and the SMTP wire-size check.
    pub max_message_bytes: u64,
    /// `None` accepts any sender domain; `Some` of an empty set denies every sender.
    pub allowed_sender_domains: Option<std::collections::HashSet<String>>,
}

const DEFAULT_MAX_MESSAGE_BYTES: u64 = 25 * 1024 * 1024;

fn parse_max_message_bytes(
    lookup: impl Fn(&str) -> Result<String, std::env::VarError>,
) -> anyhow::Result<u64> {
    const KEY: &str = "CATAPULTE_MAX_MESSAGE_BYTES";
    match lookup(KEY) {
        Err(std::env::VarError::NotPresent) => Ok(DEFAULT_MAX_MESSAGE_BYTES),
        Err(std::env::VarError::NotUnicode(_)) => anyhow::bail!("{KEY} is not valid unicode"),
        Ok(raw) => match raw.trim().parse::<u64>() {
            Ok(n) if n > 0 => Ok(n),
            _ => anyhow::bail!("{KEY} must be a positive integer, got {raw:?}"),
        },
    }
}

fn parse_allowed_sender_domains(
    lookup: impl Fn(&str) -> Result<String, std::env::VarError>,
) -> anyhow::Result<Option<std::collections::HashSet<String>>> {
    const KEY: &str = "CATAPULTE_ALLOWED_SENDER_DOMAINS";
    let raw = match lookup(KEY) {
        Err(std::env::VarError::NotPresent) => {
            tracing::warn!("{KEY} is not set: any sender domain is accepted");
            return Ok(None);
        }
        Err(std::env::VarError::NotUnicode(_)) => anyhow::bail!("{KEY} is not valid unicode"),
        Ok(raw) => raw,
    };
    let entries = raw.split(',').map(str::trim).filter(|d| !d.is_empty());
    let set = normalize_sender_domains(entries).with_context(|| format!("invalid {KEY}"))?;
    if set.is_empty() {
        tracing::warn!("{KEY} is empty: all senders denied");
    }
    Ok(Some(set))
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
        let max_message_bytes = parse_max_message_bytes(|k| std::env::var(k))?;
        let allowed_sender_domains = parse_allowed_sender_domains(|k| std::env::var(k))?;
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
            max_message_bytes,
            allowed_sender_domains,
        })
    }

    /// # Errors
    ///
    /// Returns an error when an adapter fails to build.
    #[allow(clippy::too_many_lines)]
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

        let entries = self
            .smtp
            .with_max_message_bytes(self.max_message_bytes)
            .build()
            .context("building smtp transports")?;
        let sender_configs: Vec<catapulte_domain::entity::sender::SenderConfig> = entries
            .iter()
            .map(|e| catapulte_domain::entity::sender::SenderConfig {
                name: e.name.clone(),
                quota: e.quota.clone(),
                match_sender_domain: e.match_sender_domain.clone(),
            })
            .collect();
        let routes: Vec<
            catapulte_domain::service::routed_email_sender::SenderRoute<
                catapulte_outbound_smtp::transport::SenderTransport,
            >,
        > = entries
            .into_iter()
            .map(
                |e| catapulte_domain::service::routed_email_sender::SenderRoute {
                    name: e.name,
                    priority: e.priority,
                    quota: e.quota,
                    match_sender_domain: e.match_sender_domain,
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

        let submit_email = Arc::new(
            SubmitEmailService::new(
                storage.clone(),
                queue.clone(),
                publisher.clone(),
                attachment_store.clone(),
                attachment_fetcher,
            )
            .with_limits(SubmitLimits {
                max_message_bytes: self.max_message_bytes,
                allowed_sender_domains: self.allowed_sender_domains.clone(),
            }),
        );
        let mjml_renderer = MjmlRenderer::new(self.include_loader.build());
        let process_queued_email = Arc::new(ProcessQueuedEmailService::new(
            resolver,
            MiniJinjaInterpolator::new(),
            mjml_renderer,
            smtp,
            attachment_store.clone(),
        ));

        let check_readiness = Arc::new(
            catapulte_domain::use_case::check_readiness::CheckReadinessService::new(
                crate::health::ReadinessProbe::new(storage.clone(), queue.clone()),
            ),
        );

        let state = AppState {
            submit_email,
            process_queued_email,
            list_senders,
            list_emails,
            list_events,
            check_readiness,
            queue,
            publisher,
            attachment_store: attachment_store.clone(),
            storage: storage.clone(),
        };
        let server = self.http.build();
        let worker = self.worker.build();

        let gc = gc::AttachmentGc::new(
            storage.clone(),
            attachment_store.clone(),
            self.gc_sweep_interval,
            self.gc_grace_period,
        );

        let inbound_nats_server = match self.inbound_nats {
            Some(cfg) => Some(cfg.build().await.context("building inbound NATS server")?),
            None => None,
        };

        Ok(Application {
            state,
            server,
            inbound_nats_server,
            worker,
            gc,
            metrics_enabled: false,
            metric_export_interval: Duration::from_mins(1),
        })
    }
}

pub struct Application {
    state: AppState,
    server: InboundHttpServer,
    inbound_nats_server: Option<InboundNatsServer>,
    worker: Worker,
    gc: gc::AttachmentGc,
    metrics_enabled: bool,
    metric_export_interval: Duration,
}

impl Application {
    /// Configures whether the metrics sampler should run and at what interval.
    #[must_use]
    pub fn with_metrics(mut self, enabled: bool, interval: Duration) -> Self {
        self.metrics_enabled = enabled;
        self.metric_export_interval = interval;
        self
    }

    /// # Errors
    ///
    /// Returns an error when the HTTP server fails to bind or exits unexpectedly.
    pub async fn run(self) -> anyhow::Result<()> {
        let cancel = tokio_util::sync::CancellationToken::new();
        let cancel_signal = cancel.clone();
        tokio::spawn(async move {
            let _ = tokio::signal::ctrl_c().await;
            cancel_signal.cancel();
        });
        self.run_with_shutdown(cancel).await
    }

    /// # Errors
    ///
    /// Returns an error when the HTTP server fails to bind or exits unexpectedly.
    pub async fn run_with_shutdown(
        self,
        cancel: tokio_util::sync::CancellationToken,
    ) -> anyhow::Result<()> {
        tracing::info!("catapulte starting");

        let state = self.state;
        let mut tasks: tokio::task::JoinSet<anyhow::Result<()>> = tokio::task::JoinSet::new();

        // HTTP server
        let http_cancel = cancel.clone();
        let http_state = state.clone();
        tasks.spawn(async move { self.server.run(http_state, http_cancel).await });

        // Worker
        let worker_cancel = cancel.clone();
        let worker_state = state.clone();
        tasks.spawn(async move {
            self.worker.run(worker_state, worker_cancel).await;
            Ok(())
        });

        // GC
        let gc_cancel = cancel.clone();
        tasks.spawn(async move {
            self.gc.run(gc_cancel).await;
            Ok(())
        });

        // Inbound NATS (optional)
        if let Some(inbound) = self.inbound_nats_server {
            let inb_cancel = cancel.clone();
            let inb_state = state.clone();
            tasks.spawn(async move {
                inbound.run(inb_state, inb_cancel).await;
                Ok(())
            });
        }

        // Metrics sampler (optional)
        if self.metrics_enabled {
            let sampler_queue = state.queue.clone();
            let sampler_senders = std::sync::Arc::clone(&state.list_senders);
            let sampler_backend = state.queue.backend_name();
            let sampler_interval = self.metric_export_interval;
            let sampler_cancel = cancel.clone();
            tasks.spawn(async move {
                metrics::run_sampler(
                    sampler_queue,
                    sampler_senders,
                    sampler_backend,
                    sampler_interval,
                    sampler_cancel,
                )
                .await;
                Ok(())
            });
        }

        // Wait for the cancellation signal. The ctrl_c handler (wired in run())
        // calls cancel.cancel(); tests can call it directly.
        cancel.cancelled().await;

        // Drain all tasks with a bounded timeout so in-flight work can finish.
        let drain_timeout = std::time::Duration::from_secs(30);
        let drain = async {
            while let Some(joined) = tasks.join_next().await {
                match joined {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => tracing::warn!(error = %e, "background task ended with error"),
                    Err(e) => tracing::warn!(error = %e, "background task panicked or was aborted"),
                }
            }
        };
        if tokio::time::timeout(drain_timeout, drain).await.is_err() {
            tracing::warn!("background tasks did not finish within shutdown timeout; aborting");
            tasks.abort_all();
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::env::VarError;

    use super::{parse_allowed_sender_domains, parse_max_message_bytes};

    fn lookup(value: Option<&'static str>) -> impl Fn(&str) -> Result<String, VarError> {
        move |_| value.map(str::to_owned).ok_or(VarError::NotPresent)
    }

    #[test]
    fn max_message_bytes_defaults_to_25_mib() {
        assert_eq!(parse_max_message_bytes(lookup(None)).unwrap(), 26_214_400);
    }

    #[test]
    fn max_message_bytes_parses_a_positive_integer() {
        assert_eq!(
            parse_max_message_bytes(lookup(Some("1048576"))).unwrap(),
            1_048_576
        );
    }

    #[test]
    fn max_message_bytes_rejects_zero_invalid_and_non_unicode() {
        for bad in ["0", "abc", "-1", ""] {
            assert!(
                parse_max_message_bytes(lookup(Some(bad))).is_err(),
                "{bad:?}"
            );
        }
        let non_unicode = |_: &str| Err(VarError::NotUnicode(std::ffi::OsString::from("x")));
        assert!(parse_max_message_bytes(non_unicode).is_err());
    }

    #[test]
    fn allowed_sender_domains_unset_means_no_restriction() {
        assert!(
            parse_allowed_sender_domains(lookup(None))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn allowed_sender_domains_empty_or_blank_means_deny_all() {
        for blank in ["", "   ", " , ,"] {
            let set = parse_allowed_sender_domains(lookup(Some(blank)))
                .unwrap()
                .expect("set, not None");
            assert!(set.is_empty(), "{blank:?}");
        }
    }

    #[test]
    fn allowed_sender_domains_are_normalized_and_blanks_dropped() {
        let set = parse_allowed_sender_domains(lookup(Some(" Example.com. ,, bücher.de ")))
            .unwrap()
            .unwrap();
        let expected: std::collections::HashSet<String> = ["example.com", "xn--bcher-kva.de"]
            .map(str::to_owned)
            .into();
        assert_eq!(set, expected);
    }

    #[test]
    fn allowed_sender_domains_invalid_entry_fails() {
        assert!(parse_allowed_sender_domains(lookup(Some("ok.com,bad domain\u{0}"))).is_err());
    }
}
