use catapulte::AppConfig;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();

    let app = AppConfig::from_env()?.build().await?;
    app.run().await
}

fn init_tracing() {
    use tracing_subscriber::{EnvFilter, fmt};

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    fmt().with_env_filter(filter).init();
}
