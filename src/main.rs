use clap::Parser;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

mod action;
mod controller;
mod error;
mod service;

pub(crate) fn init_logs(directive: &str, color: bool) {
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(directive))
        .with(tracing_subscriber::fmt::layer().with_ansi(color))
        .init();
}

#[cfg(test)]
pub(crate) fn try_init_logs() {
    let level = std::env::var("LOG").unwrap_or_else(|_| "catapulte=debug,tower_http=debug".into());
    let _ = tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(level))
        .with(tracing_subscriber::fmt::layer())
        .try_init();
}

#[derive(Parser)]
#[clap(about, author, version)]
struct Arguments {
    /// Log level.
    #[clap(short, long, env, default_value = "catapulte=debug,tower_http=debug")]
    pub log: String,
    /// Disable color in logs.
    #[clap(long, env, default_value = "false")]
    pub disable_log_color: bool,
    #[command(subcommand)]
    pub action: action::Action,
}

impl Arguments {
    fn init_logs(&self) {
        init_logs(&self.log, !self.disable_log_color)
    }
}

#[tokio::main]
async fn main() {
    let args = Arguments::parse();
    args.init_logs();

    args.action.execute().await
}
