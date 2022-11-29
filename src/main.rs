use clap::Parser;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

mod controller;
mod error;
mod service;

pub(crate) fn init_logs(directive: &str) {
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(directive))
        .with(tracing_subscriber::fmt::layer())
        .init();
}

#[cfg(test)]
pub(crate) fn try_init_logs() {
    let level = std::env::var("LOG").unwrap_or_else(|_| "catapulte=debug".into());
    let _ = tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(&level))
        .with(tracing_subscriber::fmt::layer())
        .try_init();
}

#[derive(Parser)]
#[clap(about, author, version)]
struct Arguments {
    /// Path to the configuration toml file, default to /etc/catapulte/catapulte.toml.
    #[clap(short, long, default_value = "/etc/catapulte/catapulte.toml")]
    pub config_path: String,
    /// Log level.
    #[clap(short, long, default_value = "INFO")]
    pub log: String,
}

impl Arguments {
    fn configuration(&self) -> Configuration {
        Configuration::parse(&self.config_path)
    }

    fn init_logs(&self) {
        init_logs(&self.log)
    }
}

#[derive(Clone, Debug, serde::Deserialize)]
struct Configuration {
    #[serde(default = "Configuration::default_host")]
    pub(crate) host: IpAddr,
    #[serde(default = "Configuration::default_port")]
    pub(crate) port: u16,
    //
    #[serde(default)]
    pub(crate) render: service::render::Configuration,
    #[serde(default)]
    pub(crate) smtp: service::smtp::Configuration,
    #[serde(default)]
    pub(crate) template: service::provider::Configuration,
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
            .add_source(config::Environment::default())
            .build()
            .unwrap()
            .try_deserialize()
            .unwrap()
    }
}

#[tokio::main]
async fn main() {
    let args = Arguments::parse();
    args.init_logs();

    let configuration = args.configuration();

    let render_options = Arc::new(configuration.render.build());
    let smtp_pool = configuration.smtp.build().expect("smtp service init");
    let template_provider = Arc::new(configuration.template.build());

    let app = controller::create(render_options, smtp_pool, template_provider);
    let address = configuration.address();

    tracing::info!("starting server on {}", address);
    axum::Server::bind(&address)
        .serve(app.into_make_service())
        .await
        .unwrap();
}

#[cfg(test)]
mod tests {
    use lazy_static;
    use serde::Deserialize;
    use uuid::Uuid;

    pub fn env_str(key: &str) -> Option<String> {
        std::env::var(key).ok()
    }

    pub fn env_number<T: std::str::FromStr>(key: &str) -> Option<T> {
        std::env::var(key)
            .ok()
            .and_then(|value| value.parse::<T>().ok())
    }

    lazy_static::lazy_static! {
        pub static ref INBOX_HOSTNAME: String =
            env_str("TEST_INBOX_HOSTNAME").unwrap_or_else(|| "localhost".to_string());
        pub static ref INBOX_PORT: u16 = env_number("TEST_INBOX_PORT").unwrap_or(1080);
    }

    #[derive(Deserialize)]
    pub struct Email {
        pub html: String,
        pub text: String,
    }

    pub async fn get_latest_inbox(from: &str, to: &str) -> Vec<Email> {
        let url = format!(
            "http://{}:{}/api/emails?from={}&to={}",
            INBOX_HOSTNAME.as_str(),
            *INBOX_PORT,
            from,
            to
        );
        reqwest::get(url.as_str())
            .await
            .unwrap()
            .json::<Vec<Email>>()
            .await
            .unwrap()
    }

    pub fn create_email() -> String {
        format!("{}@example.com", Uuid::new_v4())
    }
}
