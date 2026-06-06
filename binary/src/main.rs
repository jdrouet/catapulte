use std::time::Duration;

use catapulte::AppConfig;
use catapulte_telemetry::config::TelemetryConfig;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    if std::env::args().nth(1).as_deref() == Some("healthcheck") {
        return run_healthcheck().await;
    }

    let telemetry_cfg = TelemetryConfig::from_env("CATAPULTE_OTEL", env!("CARGO_PKG_VERSION"))?;
    let telemetry = catapulte_telemetry::telemetry::init(&telemetry_cfg)?;

    init_tracing(&telemetry);

    let app = AppConfig::from_env()?.build().await?.with_metrics(
        telemetry.metrics_enabled(),
        telemetry.metric_export_interval(),
    );
    app.run().await?;

    // Run the synchronous provider shutdown on a blocking thread so the timeout
    // can actually preempt a hung collector flush (a sync call inside a plain
    // async block has no await point for `timeout` to fire on).
    let flush = tokio::task::spawn_blocking(move || telemetry.shutdown());
    if tokio::time::timeout(Duration::from_secs(10), flush)
        .await
        .is_err()
    {
        tracing::warn!("telemetry flush timed out; skipping");
    }

    Ok(())
}

async fn run_healthcheck() -> anyhow::Result<()> {
    let url = format!("http://{}/health/ready", readiness_authority());

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()?;

    let response = client.get(&url).send().await?;

    if response.status() == reqwest::StatusCode::OK {
        Ok(())
    } else {
        anyhow::bail!("health check failed: {}", response.status())
    }
}

/// Resolves the connectable `host:port` to probe from `CATAPULTE_HTTP_ADDRESS`.
///
/// A wildcard bind (`0.0.0.0` / `[::]`) is not itself connectable, so it is
/// mapped to the loopback of the same family (`127.0.0.1` / `[::1]`). A concrete
/// bind address is probed as-is. Unparseable input falls back to `127.0.0.1:3000`.
fn readiness_authority() -> String {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

    let addr_str =
        std::env::var("CATAPULTE_HTTP_ADDRESS").unwrap_or_else(|_| String::from("127.0.0.1:3000"));

    match addr_str.parse::<SocketAddr>() {
        Ok(addr) => {
            let ip = if addr.ip().is_unspecified() {
                match addr.ip() {
                    IpAddr::V4(_) => IpAddr::V4(Ipv4Addr::LOCALHOST),
                    IpAddr::V6(_) => IpAddr::V6(Ipv6Addr::LOCALHOST),
                }
            } else {
                addr.ip()
            };
            SocketAddr::new(ip, addr.port()).to_string()
        }
        Err(_) => String::from("127.0.0.1:3000"),
    }
}

fn init_tracing(telemetry: &catapulte_telemetry::telemetry::Telemetry) {
    use tracing_subscriber::{
        EnvFilter, fmt, layer::SubscriberExt as _, util::SubscriberInitExt as _,
    };

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let fmt_layer = fmt::layer();

    tracing_subscriber::registry()
        .with(filter)
        .with(fmt_layer)
        .with(telemetry.tracing_layer())
        .init();
}
