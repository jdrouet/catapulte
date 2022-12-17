use clap::Parser;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

mod action;
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
        .with(tracing_subscriber::EnvFilter::new(level))
        .with(tracing_subscriber::fmt::layer())
        .try_init();
}

#[derive(Parser)]
#[clap(about, author, version)]
struct Arguments {
    /// Log level.
    #[clap(short, long, env, default_value = "INFO")]
    pub log: String,
    #[command(subcommand)]
    pub action: action::Action,
}

impl Arguments {
    fn init_logs(&self) {
        init_logs(&self.log)
    }
}

#[tokio::main]
async fn main() {
    let args = Arguments::parse();
    args.init_logs();

    args.action.execute().await
}

#[cfg(test)]
mod tests {
    use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
    use serde::Deserialize;
    use std::sync::Arc;
    use uuid::Uuid;

    lazy_static::lazy_static! {
        pub(crate) static ref PROMETHEUS_HANDLER: Arc<PrometheusHandle> = Arc::new(PrometheusBuilder::new()
            .install_recorder()
            .expect("failed to install prometheus recorder"));
    }

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

    pub async fn expect_latest_inbox(from: &str, kind: &str, to: &str) -> Vec<Email> {
        for _ in 0..10 {
            let list = get_latest_inbox(from, kind, to).await;
            if !list.is_empty() {
                return list;
            }
            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
        }
        panic!("mailbox is empty");
    }

    pub async fn get_latest_inbox(from: &str, kind: &str, to: &str) -> Vec<Email> {
        let url = format!(
            "http://{}:{}/api/emails?from={from}&{kind}={to}",
            INBOX_HOSTNAME.as_str(),
            *INBOX_PORT,
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
