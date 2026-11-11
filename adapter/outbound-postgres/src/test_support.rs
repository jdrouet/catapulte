//! One Postgres container per test binary, one database per test.

use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::Duration;

use crate::PostgresAdapter;

const USER: &str = "catapulte";

static SERVER_PORT: OnceLock<u16> = OnceLock::new();

/// Removes the container once this process is gone, however it ends (normal exit, panic,
/// Ctrl-C or kill): a detached shell waits for the pid to disappear, then force-removes it.
fn spawn_reaper(container_id: &str) {
    use std::os::unix::process::CommandExt as _;

    let pid = std::process::id();
    let script = format!(
        "while kill -0 {pid} 2>/dev/null; do sleep 1; done; docker rm -f -v {container_id} >/dev/null 2>&1"
    );
    let _ = Command::new("sh")
        .args(["-c", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        // Own process group, so a Ctrl-C aimed at the tests does not kill the reaper too.
        .process_group(0)
        .spawn();
}

async fn wait_for_tcp(port: u16) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .is_err()
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "127.0.0.1:{port} did not accept connections in time"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Host port of the shared server, starting it on first use. The container lives on a
/// dedicated thread with its own runtime, so it outlives the runtime of whichever test
/// started it.
fn server_port() -> u16 {
    *SERVER_PORT.get_or_init(|| {
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("postgres-container".to_owned())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("building the container runtime");
                runtime.block_on(async move {
                    use testcontainers::core::{Mount, WaitFor};
                    use testcontainers::runners::AsyncRunner;
                    use testcontainers::{GenericImage, ImageExt};

                    let container = GenericImage::new("postgres", "16-alpine")
                        .with_wait_for(WaitFor::message_on_stderr(
                            "database system is ready to accept connections",
                        ))
                        .with_env_var("POSTGRES_USER", USER)
                        .with_env_var("POSTGRES_PASSWORD", USER)
                        // Data in memory: faster tests and no anonymous volume to delete afterwards.
                        .with_mount(Mount::tmpfs_mount("/var/lib/postgresql/data"))
                        .with_cmd(["postgres", "-c", "max_connections=500", "-c", "fsync=off"])
                        .start()
                        .await
                        .expect("failed to start postgres container; ensure Docker is running");
                    spawn_reaper(container.id());
                    let port = container.get_host_port_ipv4(5432u16).await.unwrap();
                    wait_for_tcp(port).await;
                    sender.send(port).unwrap();
                    // Keeps the container (and this runtime) alive for the whole process.
                    std::future::pending::<()>().await;
                });
            })
            .expect("spawning the container thread");
        receiver
            .recv()
            .expect("the container thread reported no port")
    })
}

/// A migrated adapter on a database no other test uses.
pub async fn fresh_adapter() -> PostgresAdapter {
    let port = server_port();
    let database = format!("test_{}", uuid::Uuid::now_v7().simple());
    let server = format!("postgres://{USER}:{USER}@127.0.0.1:{port}");

    let admin = PostgresAdapter::connect(&format!("{server}/postgres"), 1, Duration::from_secs(30))
        .await
        .unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {database}")))
        .execute(admin.pool())
        .await
        .unwrap();

    let adapter =
        PostgresAdapter::connect(&format!("{server}/{database}"), 10, Duration::from_secs(30))
            .await
            .unwrap();
    adapter.migrate().await.unwrap();
    adapter
}
