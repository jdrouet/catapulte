use crate::service::provider::TemplateProvider;
use crate::service::render::RenderOptions;
use crate::service::smtp::SmtpPool;
use metrics_exporter_prometheus::PrometheusBuilder;
use metrics_exporter_prometheus::PrometheusHandle;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use tokio::net::TcpListener;

#[derive(Clone, Debug, serde::Deserialize)]
pub(crate) struct Configuration {
    #[serde(default = "Configuration::default_host")]
    pub(crate) host: IpAddr,
    #[serde(default = "Configuration::default_port")]
    pub(crate) port: u16,
    //
    #[serde(default)]
    pub(crate) render: crate::service::render::Configuration,
    #[serde(default)]
    pub(crate) smtp: crate::service::smtp::Configuration,
    #[serde(default)]
    pub(crate) template: crate::service::provider::Configuration,
}

impl Configuration {
    fn default_host() -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))
    }

    fn default_port() -> u16 {
        3000
    }

    fn address(&self) -> SocketAddr {
        SocketAddr::from((self.host, self.port))
    }

    pub(crate) fn from_path(path: &str) -> Self {
        config::Config::builder()
            .add_source(config::File::with_name(path).required(false))
            .add_source(config::Environment::default().separator("__"))
            .build()
            .unwrap()
            .try_deserialize()
            .unwrap()
    }
}

pub(crate) struct Server {
    socket_address: SocketAddr,
    render_options: RenderOptions,
    smtp_pool: SmtpPool,
    template_provider: TemplateProvider,
    prometheus_handle: PrometheusHandle,
}

impl Server {
    pub fn new(
        socket_address: SocketAddr,
        render_options: RenderOptions,
        smtp_pool: SmtpPool,
        template_provider: TemplateProvider,
        prometheus_handle: PrometheusHandle,
    ) -> Self {
        Self {
            socket_address,
            render_options,
            smtp_pool,
            template_provider,
            prometheus_handle,
        }
    }

    pub fn from_config(config: Configuration) -> Self {
        Self::new(
            config.address(),
            config.render.build(),
            config.smtp.build().expect("smtp service init"),
            config.template.build(),
            PrometheusBuilder::new()
                .install_recorder()
                .expect("failed to install prometheus recorder"),
        )
    }

    pub fn app(self) -> axum::Router {
        use axum::extract::Extension;

        crate::controller::create()
            .layer(Extension(Arc::new(self.render_options)))
            .layer(Extension(self.smtp_pool))
            .layer(Extension(self.template_provider))
            .layer(Extension(Arc::new(self.prometheus_handle)))
            .layer(tower_http::trace::TraceLayer::new_for_http())
    }

    pub async fn run(self) {
        tracing::info!("starting server on {:?}", self.socket_address);
        let tcp_listener = TcpListener::bind(&self.socket_address).await.unwrap();

        axum::serve(tcp_listener, self.app().into_make_service())
            .with_graceful_shutdown(shutdown_signal())
            .await
            .unwrap();
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

#[cfg(test)]
pub(crate) mod tests {
    // TODO
}
