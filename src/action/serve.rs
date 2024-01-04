use metrics_exporter_prometheus::PrometheusBuilder;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use tokio::net::TcpListener;

#[derive(Clone, Debug, serde::Deserialize)]
struct Configuration {
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
    #[serde(default)]
    pub(crate) tracing: crate::service::tracing::Configuration,
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

    pub(crate) fn parse(path: &str) -> Self {
        config::Config::builder()
            .add_source(config::File::with_name(path).required(false))
            .add_source(config::Environment::default().separator("__"))
            .build()
            .unwrap()
            .try_deserialize()
            .unwrap()
    }
}

#[derive(clap::Parser)]
pub(crate) struct Action {
    /// Path to the configuration toml file, default to /etc/catapulte/catapulte.toml.
    #[clap(short, long, default_value = "/etc/catapulte/catapulte.toml")]
    pub config_path: String,
}

impl Action {
    fn configuration(&self) -> Configuration {
        Configuration::parse(&self.config_path)
    }

    pub(crate) async fn execute(self) {
        let configuration = self.configuration();

        let render_options = Arc::new(configuration.render.build());
        let smtp_pool = configuration.smtp.build().expect("smtp service init");
        let template_provider = Arc::new(configuration.template.build());
        let prometheus = Arc::new(
            PrometheusBuilder::new()
                .install_recorder()
                .expect("failed to install prometheus recorder"),
        );

        let address = configuration.address();

        let app =
            crate::controller::create(render_options, smtp_pool, template_provider, prometheus);
        let app = configuration.tracing.add_layer(app);

        tracing::info!("starting server on {}", address);
        let tcp_listener = TcpListener::bind(&address).await.unwrap();
        axum::serve(tcp_listener, app.into_make_service())
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
