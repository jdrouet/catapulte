use catapulte::AppConfig;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    if std::env::args().nth(1).as_deref() == Some("healthcheck") {
        return run_healthcheck().await;
    }

    init_tracing();

    let app = AppConfig::from_env()?.build().await?;
    app.run().await
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

fn init_tracing() {
    use tracing_subscriber::{EnvFilter, fmt};

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    fmt().with_env_filter(filter).init();
}
