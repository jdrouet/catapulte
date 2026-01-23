use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;

use axum::Router;
use catapulte_domain::prelude::{EmailSender, TemplateLoader, TemplateRenderer};
use catapulte_domain::service::SendEmailService;
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use tokio::net::TcpListener;

use crate::controller;

/// HTTP server configuration
#[derive(Clone, Debug, serde::Deserialize)]
pub struct HttpConfig {
    #[serde(default = "HttpConfig::default_host")]
    pub host: IpAddr,
    #[serde(default = "HttpConfig::default_port")]
    pub port: u16,
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            host: Self::default_host(),
            port: Self::default_port(),
        }
    }
}

impl HttpConfig {
    fn default_host() -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))
    }

    fn default_port() -> u16 {
        3000
    }

    fn address(&self) -> SocketAddr {
        SocketAddr::from((self.host, self.port))
    }
}

/// HTTP server wrapping the email service
pub struct HttpServer<L, R, S>
where
    L: TemplateLoader + 'static,
    R: TemplateRenderer + 'static,
    S: EmailSender + 'static,
{
    config: HttpConfig,
    service: Arc<SendEmailService<L, R, S>>,
    prometheus_handle: PrometheusHandle,
}

impl<L, R, S> HttpServer<L, R, S>
where
    L: TemplateLoader + 'static,
    R: TemplateRenderer + 'static,
    S: EmailSender + 'static,
{
    pub fn new(config: HttpConfig, service: SendEmailService<L, R, S>) -> Self {
        let prometheus_handle = PrometheusBuilder::new()
            .install_recorder()
            .expect("failed to install prometheus recorder");

        Self {
            config,
            service: Arc::new(service),
            prometheus_handle,
        }
    }

    /// Build the Axum router
    pub fn router(&self) -> Router {
        controller::create_router(self.service.clone(), self.prometheus_handle.clone())
    }

    /// Run the HTTP server
    pub async fn run(self) {
        let address = self.config.address();
        tracing::info!("starting server on {:?}", address);

        let tcp_listener = TcpListener::bind(&address)
            .await
            .expect("failed to bind TCP listener");

        axum::serve(tcp_listener, self.router().into_make_service())
            .with_graceful_shutdown(shutdown_signal())
            .await
            .expect("server error");
    }
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    tracing::info!("signal received, starting graceful shutdown");
}
