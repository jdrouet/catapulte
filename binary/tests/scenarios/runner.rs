use std::future::Future;
use std::net::TcpListener;
use std::time::Duration;

use testcontainers::GenericImage;
use testcontainers::core::{IntoContainerPort, WaitFor};
use testcontainers::runners::AsyncRunner;
use tokio_util::sync::CancellationToken;

use crate::scenarios::backends::BackendBundle;
use crate::scenarios::context::TestContext;

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Polls a `127.0.0.1:<port>` TCP listener until it accepts a connection or the
/// deadline elapses. Used to confirm a container's mapped port is actually
/// reachable, not merely logged as starting.
async fn wait_for_tcp(port: u16, timeout: Duration) {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_ok()
        {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "127.0.0.1:{port} did not accept connections within {timeout:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn start_mailpit() -> testcontainers::ContainerAsync<GenericImage> {
    GenericImage::new("axllent/mailpit", "latest")
        .with_exposed_port(1025.tcp())
        .with_exposed_port(8025.tcp())
        .with_wait_for(WaitFor::message_on_stdout("[http] accessible via"))
        .start()
        .await
        .expect("failed to start mailpit container; ensure Docker is running")
}

pub async fn run_scenario<S, SFut, B, BFut>(
    scenario_name: &str,
    scenario: S,
    backend_name: &str,
    backend: B,
) where
    S: FnOnce(TestContext) -> SFut,
    SFut: Future<Output = ()>,
    B: FnOnce(u16, u16) -> BFut,
    BFut: Future<Output = BackendBundle>,
{
    let mailpit = start_mailpit().await;
    let smtp_port = mailpit.get_host_port_ipv4(1025).await.unwrap();
    let api_port = mailpit.get_host_port_ipv4(8025).await.unwrap();

    // Mailpit logs its HTTP listener as ready before the Docker host-port proxy
    // reliably accepts SMTP connections on the mapped port. The memory queue
    // delivers the instant an email is submitted, so the worker can race that
    // window and hit `connection refused`; the resulting transient failure is
    // retried only after a 30s backoff, well past a scenario's poll window.
    // Probe the mapped SMTP port with a real TCP connection so submissions only
    // start once delivery can actually succeed.
    wait_for_tcp(smtp_port, Duration::from_secs(15)).await;

    let http_port = free_port();

    let bundle = backend(smtp_port, http_port).await;

    let cancel = CancellationToken::new();
    let app = bundle
        .config
        .build()
        .await
        .expect("failed to build application");

    let cancel_app = cancel.clone();
    let app_task = tokio::spawn(async move { app.run_with_shutdown(cancel_app).await });

    // Poll until the HTTP server is ready.
    let client = reqwest::Client::new();
    let http_base = format!("http://127.0.0.1:{http_port}");
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    let mut ready = false;
    while std::time::Instant::now() < deadline {
        if let Ok(resp) = client.get(format!("{http_base}/health/ready")).send().await
            && resp.status().is_success()
        {
            ready = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        ready,
        "{scenario_name}__{backend_name}: server did not become ready within 15s"
    );

    let ctx = TestContext {
        client,
        http_base,
        mailpit_api_base: format!("http://127.0.0.1:{api_port}"),
    };

    scenario(ctx).await;

    cancel.cancel();
    match tokio::time::timeout(Duration::from_secs(15), app_task).await {
        Ok(Ok(Ok(()))) => {}
        Ok(Ok(Err(e))) => {
            tracing::warn!("{scenario_name}__{backend_name}: app exited with error: {e}");
        }
        Ok(Err(join_err)) => {
            tracing::warn!("{scenario_name}__{backend_name}: app task panicked: {join_err}");
        }
        Err(_) => {
            tracing::warn!(
                "{scenario_name}__{backend_name}: app did not shut down within 15s; dropping anyway"
            );
        }
    }

    // Drop guards after we've awaited the app task.
    drop(bundle.drop_guards);
    drop(mailpit);
}

#[macro_export]
macro_rules! e2e_matrix {
    // Entry point: capture the entire backends list as a tt so the outer
    // scenario repetition doesn't try to match $backend counts against it.
    (scenarios: [$($scenario:ident),* $(,)?], backends: $backends:tt $(,)?) => {
        $(
            $crate::e2e_matrix!(@scenario $scenario, backends: $backends);
        )*
    };
    // Inner rule: for one scenario, generate one test per backend.
    (@scenario $scenario:ident, backends: [$($backend:ident),* $(,)?]) => {
        $(
            paste::paste! {
                #[allow(non_snake_case)]
                #[tokio::test(flavor = "multi_thread")]
                #[serial_test::serial]
                async fn [<$scenario __ $backend>]() {
                    $crate::scenarios::runner::run_scenario(
                        stringify!($scenario),
                        $crate::scenarios::list::$scenario::scenario,
                        stringify!($backend),
                        $crate::scenarios::backends::$backend,
                    ).await;
                }
            }
        )*
    };
}
