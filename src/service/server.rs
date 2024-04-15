use crate::service::smtp::SmtpPool;
use metrics_exporter_prometheus::PrometheusBuilder;
use metrics_exporter_prometheus::PrometheusHandle;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use tokio::net::TcpListener;

#[derive(Clone, Debug, serde::Deserialize)]
pub struct Configuration {
    #[serde(default = "Configuration::default_host")]
    pub host: IpAddr,
    #[serde(default = "Configuration::default_port")]
    pub port: u16,
    //
    #[serde(default)]
    pub smtp: crate::service::smtp::Configuration,
    #[serde(flatten)]
    pub engine: catapulte_engine::Config,
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

    pub fn from_path(path: &str) -> Self {
        config::Config::builder()
            .add_source(config::File::with_name(path).required(false))
            .add_source(config::Environment::default().separator("__"))
            .build()
            .unwrap()
            .try_deserialize()
            .unwrap()
    }
}

pub struct Server {
    socket_address: SocketAddr,
    smtp_pool: SmtpPool,
    prometheus_handle: PrometheusHandle,
    engine: catapulte_engine::Engine,
}

#[cfg(test)]
impl Server {
    pub fn default_insecure() -> Self {
        let smtp_pool = crate::service::smtp::Configuration::insecure()
            .build()
            .unwrap();
        let prometheus_handle = PrometheusBuilder::new().build_recorder().handle();
        let engine = catapulte_engine::Config::default().into();

        Server::new(
            SocketAddr::from((IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 5555)),
            smtp_pool,
            prometheus_handle,
            engine,
        )
    }
}

impl Server {
    #[inline]
    pub(crate) fn new(
        socket_address: SocketAddr,
        smtp_pool: SmtpPool,
        prometheus_handle: PrometheusHandle,
        engine: catapulte_engine::Engine,
    ) -> Self {
        Self {
            socket_address,
            smtp_pool,
            prometheus_handle,
            engine,
        }
    }

    pub fn from_config(config: Configuration) -> Self {
        Self::new(
            config.address(),
            config.smtp.build().expect("smtp service init"),
            PrometheusBuilder::new()
                .install_recorder()
                .expect("failed to install prometheus recorder"),
            config.engine.into(),
        )
    }

    pub(crate) fn app(self) -> axum::Router {
        use axum::extract::Extension;

        crate::controller::create()
            .layer(Extension(self.smtp_pool))
            .layer(Extension(self.prometheus_handle))
            .layer(Extension(self.engine))
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
