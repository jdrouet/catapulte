use std::time::Duration;

use catapulte::AppConfig;
use catapulte_telemetry::config::TelemetryConfig;

fn main() -> anyhow::Result<()> {
    // The render child must start before any runtime, telemetry or tracing setup.
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some(catapulte::render_sandbox::SANDBOX_ARG) {
        std::process::exit(catapulte::render_sandbox::run_child(&args[2..]));
    }
    serve()
}

#[tokio::main]
async fn serve() -> anyhow::Result<()> {
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
        .with(nats_wire_trace_cap())
        .with(fmt_layer)
        .with(telemetry.tracing_layer())
        .init();
}

// async-nats logs raw wire frames (payload and headers) at TRACE on this target,
// so they are dropped regardless of RUST_LOG.
fn nats_wire_trace_cap()
-> tracing_subscriber::filter::FilterFn<impl Fn(&tracing::Metadata<'_>) -> bool> {
    tracing_subscriber::filter::filter_fn(|meta| {
        let target = meta.target();
        let wire =
            target == "async_nats::connection" || target.starts_with("async_nats::connection::");
        !(*meta.level() == tracing::Level::TRACE && wire)
    })
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::sync::{Arc, Mutex};

    use tracing::{debug, trace};
    use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt as _, registry};

    use super::nats_wire_trace_cap;

    #[derive(Clone)]
    struct Buf(Arc<Mutex<Vec<u8>>>);

    impl io::Write for Buf {
        fn write(&mut self, data: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(data);
            Ok(data.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn emit() {
        trace!(target: "async_nats::connection", op = "CONN_TRACE_MARKER", "wire");
        debug!(target: "async_nats::connection", op = "CONN_DEBUG_MARKER", "wire");
        trace!(target: "async_nats::jetstream", op = "JS_TRACE_MARKER", "js");
    }

    #[test]
    fn nats_wire_trace_is_dropped_whatever_the_env_filter_says() {
        let cases = [
            ("trace", [false, true, true]),
            ("async_nats=trace", [false, true, true]),
            ("async_nats::connection=trace", [false, true, false]),
            (
                "trace,async_nats::connection[{op}]=trace",
                [false, true, true],
            ),
            ("warn", [false, false, false]),
        ];
        let markers = ["CONN_TRACE_MARKER", "CONN_DEBUG_MARKER", "JS_TRACE_MARKER"];
        for (input, expected) in cases {
            let buf = Buf(Arc::default());
            let writer = buf.clone();
            let subscriber = registry()
                .with(EnvFilter::try_new(input).unwrap())
                .with(nats_wire_trace_cap())
                .with(
                    fmt::layer()
                        .with_writer(move || writer.clone())
                        .with_ansi(false),
                );
            tracing::subscriber::with_default(subscriber, emit);
            let out = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
            for (marker, want) in markers.iter().zip(expected) {
                assert_eq!(
                    out.contains(marker),
                    want,
                    "input {input:?}, marker {marker}"
                );
            }
        }
    }
}
