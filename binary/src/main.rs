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
        .with(LibraryLogGuard)
        .with(fmt_layer)
        .with(telemetry.tracing_layer())
        .init();
}

/// Drops library log lines that echo message content, broker error text or
/// request hosts, whatever `RUST_LOG` says. Every other library line is kept.
struct LibraryLogGuard;

fn in_module(target: &str, module: &str) -> bool {
    target
        .strip_prefix(module)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with("::"))
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for LibraryLogGuard {
    fn enabled(
        &self,
        meta: &tracing::Metadata<'_>,
        _: tracing_subscriber::layer::Context<'_, S>,
    ) -> bool {
        let target = meta.target();
        let trace = *meta.level() == tracing::Level::TRACE;
        // Wire frames, and connection/pool lines naming the remote host.
        let leaky = (trace
            && (in_module(target, "async_nats::connection")
                || in_module(target, "hyper_util::client::legacy::connect::http")))
            || in_module(target, "hyper_util::client::legacy::pool")
            || in_module(target, "reqwest::connect");
        !leaky
    }

    // These async-nats callsites also log harmless events (`event: connected`),
    // so only the lines that carry the broker's `-ERR` text are matched.
    fn event_enabled(
        &self,
        event: &tracing::Event<'_>,
        _: tracing_subscriber::layer::Context<'_, S>,
    ) -> bool {
        if event.metadata().target() != "async_nats" {
            return true;
        }
        let mut message = Message(String::new());
        event.record(&mut message);
        !(message.0.starts_with("event: server error") || message.0.starts_with("received ERROR"))
    }
}

struct Message(String);

impl tracing::field::Visit for Message {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0 = format!("{value:?}");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::sync::{Arc, Mutex};

    use tracing::{debug, trace};
    use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt as _, registry};

    use super::LibraryLogGuard;

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
                .with(LibraryLogGuard)
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

    // Capture needs the production subscriber installed process-wide (with its log
    // bridge) before any other test touches the callsites, so it runs in a child.
    #[test]
    fn library_logs_never_leak_broker_error_or_webhook_url() {
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tests::library_log_capture_in_isolated_process",
                "--ignored",
                "--test-threads=1",
                "--nocapture",
            ])
            .output()
            .unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(out.status.success(), "child failed: {text}");
        assert!(text.contains("1 passed"), "child ran no test: {text}");
    }

    #[tokio::test]
    #[ignore = "run in its own process by library_logs_never_leak_broker_error_or_webhook_url"]
    async fn library_log_capture_in_isolated_process() {
        use catapulte_domain::entity::email::EmailId;
        use catapulte_domain::entity::lifecycle_event::{EventContext, LifecycleEvent};
        use catapulte_domain::port::event_publisher::EventPublisher as _;
        use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
        use tracing_subscriber::util::SubscriberInitExt as _;

        let buf = Buf(Arc::default());
        let writer = buf.clone();
        registry()
            .with(EnvFilter::new("trace"))
            .with(LibraryLogGuard)
            .with(
                fmt::layer()
                    .with_writer(move || writer.clone())
                    .with_ansi(false),
            )
            .init();

        // A broker that accepts the connection, then reports an error carrying the marker.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let nats_addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let (read, mut write) = socket.into_split();
            write
                .write_all(b"INFO {\"server_id\":\"fake\",\"version\":\"2.10.0\",\"proto\":1,\"max_payload\":1048576,\"headers\":true}\r\n")
                .await
                .unwrap();
            let mut lines = BufReader::new(read).lines();
            while let Some(line) = lines.next_line().await.unwrap() {
                if line == "PING" {
                    write.write_all(b"PONG\r\n").await.unwrap();
                    write
                        .write_all(b"-ERR 'BROKERMARKER-871 for victim@example.com'\r\n")
                        .await
                        .unwrap();
                }
            }
        });
        let (seen_tx, mut seen_rx) = tokio::sync::mpsc::unbounded_channel();
        let _client = async_nats::ConnectOptions::new()
            .event_callback(move |event| {
                let seen_tx = seen_tx.clone();
                async move {
                    if matches!(event, async_nats::Event::ServerError(_)) {
                        let _ = seen_tx.send(());
                    }
                }
            })
            .connect(nats_addr.to_string())
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(10), seen_rx.recv())
            .await
            .expect("broker error never reached the client");

        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(wiremock::ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        let client = reqwest::Client::builder()
            .no_proxy()
            .resolve("hostmarker-871.test", *server.address())
            .build()
            .unwrap();
        let url = reqwest::Url::parse(&format!(
            "http://usermarker-871:passmarker-871@hostmarker-871.test:{}/pathmarker-871?q=querymarker-871",
            server.address().port()
        ))
        .unwrap();
        catapulte_outbound_webhook::WebhookPublisher::new(client, url)
            .publish(&LifecycleEvent::Queued {
                id: EmailId::default(),
                context: EventContext::with_correlation(None),
            })
            .await
            .unwrap();
        server.verify().await;

        let logs = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
        for marker in [
            "BROKERMARKER-871",
            "victim@example.com",
            "hostmarker-871",
            "usermarker-871",
            "passmarker-871",
            "pathmarker-871",
            "querymarker-871",
        ] {
            assert!(!logs.contains(marker), "{marker} leaked: {logs}");
        }
        // The rest of the library output still flows.
        assert!(
            logs.contains("event: connected"),
            "nats events dropped: {logs}"
        );
        assert!(
            logs.contains("hyper_util::client::legacy::connect::http: connecting to"),
            "http connection lines dropped: {logs}"
        );
    }
}
