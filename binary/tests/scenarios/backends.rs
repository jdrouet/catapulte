use std::any::Any;
use std::collections::HashSet;
use std::sync::OnceLock;

use catapulte::AppConfig;
use catapulte::attachment_store::AttachmentStoreBackendConfig;
use catapulte::publisher::PublisherAdapterConfig;
use catapulte::queue::QueueBackendConfig;
use catapulte::storage::StorageBackendConfig;
use catapulte_inbound_http::InboundHttpConfig;
use catapulte_inbound_worker::worker::WorkerConfig;
use catapulte_outbound_attachment_fetcher::fetcher::HttpAttachmentFetcherConfig;
use catapulte_outbound_attachment_fs::store::FsAttachmentStoreConfig;
use catapulte_outbound_nats::NatsConfig;
use catapulte_outbound_postgres::PostgresConfig;
use catapulte_outbound_resolver::resolver::TemplateResolverConfig;
use catapulte_outbound_smtp::multi_sender::MultiSenderConfig;
use catapulte_outbound_smtp::transport::{SmtpConfig, SmtpTls};
use catapulte_outbound_sqlite::SqliteConfig;
use testcontainers::GenericImage;
use testcontainers::ImageExt;
use testcontainers::core::{IntoContainerPort, Mount, WaitFor};
use testcontainers::runners::AsyncRunner;

pub struct BackendBundle {
    pub config: AppConfig,
    pub drop_guards: Vec<Box<dyn Any + Send>>,
}

async fn wait_for_tcp(port: u16, timeout: std::time::Duration) {
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
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

fn base_smtp(port: u16) -> MultiSenderConfig {
    MultiSenderConfig::single(
        "default",
        SmtpConfig {
            host: "127.0.0.1".to_owned(),
            port,
            username: None,
            password: None,
            tls: SmtpTls::None,
        },
    )
}

fn base_resolver() -> TemplateResolverConfig {
    TemplateResolverConfig {
        allowed_domains: HashSet::new(),
        templates_dir: None,
        auth_entries: Vec::new(),
        fetch_timeout: std::time::Duration::from_secs(30),
        max_bytes: 1_048_576,
    }
}

fn base_attachment_fetcher() -> HttpAttachmentFetcherConfig {
    HttpAttachmentFetcherConfig::default()
}

static POSTGRES: OnceLock<(String, u16)> = OnceLock::new();

// The container lives on its own thread and runtime so it outlives the test that started it.
fn postgres() -> &'static (String, u16) {
    POSTGRES.get_or_init(|| {
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("postgres-container".to_owned())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("building the container runtime");
                runtime.block_on(async move {
                    let container = crate::support::labelled(
                        GenericImage::new("postgres", "16-alpine")
                            .with_exposed_port(5432.tcp())
                            .with_wait_for(WaitFor::message_on_stderr(
                                "database system is ready to accept connections",
                            ))
                            .with_env_var("POSTGRES_USER", "catapulte")
                            .with_env_var("POSTGRES_PASSWORD", "catapulte")
                            .with_env_var("POSTGRES_DB", "catapulte")
                            .with_mount(Mount::tmpfs_mount("/var/lib/postgresql/data"))
                            .with_cmd(["postgres", "-c", "max_connections=500", "-c", "fsync=off"]),
                    )
                    .start()
                    .await
                    .expect("failed to start postgres container; ensure Docker is running");
                    let port = container.get_host_port_ipv4(5432).await.unwrap();
                    wait_for_tcp(port, std::time::Duration::from_secs(30)).await;
                    sender.send((container.id().to_owned(), port)).unwrap();
                    // Keeps the container alive for the whole process.
                    std::future::pending::<()>().await;
                });
            })
            .expect("spawning the container thread");
        receiver
            .recv()
            .expect("the container thread reported no server")
    })
}

fn fresh_database_url() -> String {
    let (id, port) = postgres();
    let name = format!("test_{}", uuid::Uuid::now_v7().simple());
    let output = std::process::Command::new("docker")
        .args([
            "exec",
            id,
            "psql",
            "-U",
            "catapulte",
            "-d",
            "catapulte",
            "-c",
        ])
        .arg(format!("CREATE DATABASE {name}"))
        .output()
        .expect("failed to run docker exec");
    assert!(
        output.status.success(),
        "CREATE DATABASE failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    format!("postgres://catapulte:catapulte@127.0.0.1:{port}/{name}")
}

async fn start_nats() -> testcontainers::ContainerAsync<GenericImage> {
    crate::support::labelled(
        GenericImage::new("nats", "2-alpine")
            .with_exposed_port(4222.tcp())
            .with_wait_for(WaitFor::message_on_stderr("Server is ready"))
            .with_wait_for(WaitFor::millis(200))
            .with_cmd(["nats-server".to_owned(), "--js".to_owned()]),
    )
    .start()
    .await
    .expect("failed to start NATS container; ensure Docker is running")
}

fn nats_config(url: String, http_port: u16) -> NatsConfig {
    let tag = http_port;
    NatsConfig {
        url,
        stream: format!("CATAPULTE_E2E_{tag}"),
        subject: format!("catapulte.e2e.{tag}.queued"),
        consumer: format!("e2e-worker-{tag}"),
        ack_wait_secs: 5,
        max_deliver: 3,
        backoff_secs: vec![1, 2, 3],
    }
}

fn sqlite_config(smtp_port: u16, http_port: u16, db_tag: &str) -> (AppConfig, tempfile::TempDir) {
    let db_dir = tempfile::tempdir().unwrap();
    let db_path = db_dir.path().join(format!("{db_tag}.db"));
    let attachment_dir_path = std::env::temp_dir().join(format!("catapulte_scenarios_{http_port}"));
    let config = AppConfig {
        storage: StorageBackendConfig::Sqlite(SqliteConfig {
            url: format!("sqlite:{}", db_path.display()),
        }),
        http: InboundHttpConfig {
            address: format!("127.0.0.1:{http_port}").parse().unwrap(),
            auth: None,
            feedback_secret: None,
            request_timeout: std::time::Duration::from_secs(30),
        },
        inbound_nats: None,
        smtp: base_smtp(smtp_port),
        resolver: base_resolver(),
        worker: WorkerConfig::default(),
        queue: QueueBackendConfig::Storage,
        publisher: PublisherAdapterConfig::storage_only(),
        attachment_store: AttachmentStoreBackendConfig::Fs(FsAttachmentStoreConfig {
            root: attachment_dir_path,
        }),
        attachment_fetcher: base_attachment_fetcher(),
        include_loader: catapulte_outbound_mjml::include_loader::IncludeLoaderConfig::default(),
        render: catapulte::render_sandbox::RenderSandboxConfig::new(
            env!("CARGO_BIN_EXE_catapulte").into(),
        ),
        gc_sweep_interval: std::time::Duration::from_hours(1),
        gc_grace_period: std::time::Duration::from_hours(1),
        max_message_bytes: 26_214_400,
        max_schedule_ahead: std::time::Duration::from_hours(720),
        allowed_sender_domains: None,
        bounce: None,
    };
    (config, db_dir)
}

pub async fn sqlite_storage(smtp_port: u16, http_port: u16) -> BackendBundle {
    let (config, db_dir) = sqlite_config(smtp_port, http_port, "scenarios_sqlite_storage");
    BackendBundle {
        config,
        drop_guards: vec![Box::new(db_dir)],
    }
}

pub async fn sqlite_memory(smtp_port: u16, http_port: u16) -> BackendBundle {
    let db_dir = tempfile::tempdir().unwrap();
    let db_path = db_dir.path().join("scenarios_sqlite_memory.db");
    let attachment_dir_path =
        std::env::temp_dir().join(format!("catapulte_scenarios_mem_{http_port}"));
    let config = AppConfig {
        storage: StorageBackendConfig::Sqlite(SqliteConfig {
            url: format!("sqlite:{}", db_path.display()),
        }),
        http: InboundHttpConfig {
            address: format!("127.0.0.1:{http_port}").parse().unwrap(),
            auth: None,
            feedback_secret: None,
            request_timeout: std::time::Duration::from_secs(30),
        },
        inbound_nats: None,
        smtp: base_smtp(smtp_port),
        resolver: base_resolver(),
        worker: WorkerConfig::default(),
        queue: QueueBackendConfig::Memory,
        publisher: PublisherAdapterConfig::storage_only(),
        attachment_store: AttachmentStoreBackendConfig::Fs(FsAttachmentStoreConfig {
            root: attachment_dir_path,
        }),
        attachment_fetcher: base_attachment_fetcher(),
        include_loader: catapulte_outbound_mjml::include_loader::IncludeLoaderConfig::default(),
        render: catapulte::render_sandbox::RenderSandboxConfig::new(
            env!("CARGO_BIN_EXE_catapulte").into(),
        ),
        gc_sweep_interval: std::time::Duration::from_hours(1),
        gc_grace_period: std::time::Duration::from_hours(1),
        max_message_bytes: 26_214_400,
        max_schedule_ahead: std::time::Duration::from_hours(720),
        allowed_sender_domains: None,
        bounce: None,
    };
    BackendBundle {
        config,
        drop_guards: vec![Box::new(db_dir)],
    }
}

pub async fn sqlite_nats(smtp_port: u16, http_port: u16) -> BackendBundle {
    let nats_container = start_nats().await;
    let nats_port = nats_container.get_host_port_ipv4(4222).await.unwrap();
    wait_for_tcp(nats_port, std::time::Duration::from_secs(15)).await;

    let db_dir = tempfile::tempdir().unwrap();
    let db_path = db_dir.path().join("scenarios_sqlite_nats.db");
    let attachment_dir_path =
        std::env::temp_dir().join(format!("catapulte_scenarios_nats_{http_port}"));
    let config = AppConfig {
        storage: StorageBackendConfig::Sqlite(SqliteConfig {
            url: format!("sqlite:{}", db_path.display()),
        }),
        http: InboundHttpConfig {
            address: format!("127.0.0.1:{http_port}").parse().unwrap(),
            auth: None,
            feedback_secret: None,
            request_timeout: std::time::Duration::from_secs(30),
        },
        inbound_nats: None,
        smtp: base_smtp(smtp_port),
        resolver: base_resolver(),
        worker: WorkerConfig::default(),
        queue: QueueBackendConfig::Nats(nats_config(
            format!("nats://127.0.0.1:{nats_port}"),
            http_port,
        )),
        publisher: PublisherAdapterConfig::storage_only(),
        attachment_store: AttachmentStoreBackendConfig::Fs(FsAttachmentStoreConfig {
            root: attachment_dir_path,
        }),
        attachment_fetcher: base_attachment_fetcher(),
        include_loader: catapulte_outbound_mjml::include_loader::IncludeLoaderConfig::default(),
        render: catapulte::render_sandbox::RenderSandboxConfig::new(
            env!("CARGO_BIN_EXE_catapulte").into(),
        ),
        gc_sweep_interval: std::time::Duration::from_hours(1),
        gc_grace_period: std::time::Duration::from_hours(1),
        max_message_bytes: 26_214_400,
        max_schedule_ahead: std::time::Duration::from_hours(720),
        allowed_sender_domains: None,
        bounce: None,
    };
    BackendBundle {
        config,
        drop_guards: vec![Box::new(db_dir), Box::new(nats_container)],
    }
}

pub async fn postgres_storage(smtp_port: u16, http_port: u16) -> BackendBundle {
    let attachment_dir_path =
        std::env::temp_dir().join(format!("catapulte_scenarios_pg_{http_port}"));
    let config = AppConfig {
        storage: StorageBackendConfig::Postgres(PostgresConfig {
            url: fresh_database_url(),
            max_connections: 10,
            acquire_timeout: std::time::Duration::from_secs(30),
        }),
        http: InboundHttpConfig {
            address: format!("127.0.0.1:{http_port}").parse().unwrap(),
            auth: None,
            feedback_secret: None,
            request_timeout: std::time::Duration::from_secs(30),
        },
        inbound_nats: None,
        smtp: base_smtp(smtp_port),
        resolver: base_resolver(),
        worker: WorkerConfig::default(),
        queue: QueueBackendConfig::Storage,
        publisher: PublisherAdapterConfig::storage_only(),
        attachment_store: AttachmentStoreBackendConfig::Fs(FsAttachmentStoreConfig {
            root: attachment_dir_path,
        }),
        attachment_fetcher: base_attachment_fetcher(),
        include_loader: catapulte_outbound_mjml::include_loader::IncludeLoaderConfig::default(),
        render: catapulte::render_sandbox::RenderSandboxConfig::new(
            env!("CARGO_BIN_EXE_catapulte").into(),
        ),
        gc_sweep_interval: std::time::Duration::from_hours(1),
        gc_grace_period: std::time::Duration::from_hours(1),
        max_message_bytes: 26_214_400,
        max_schedule_ahead: std::time::Duration::from_hours(720),
        allowed_sender_domains: None,
        bounce: None,
    };
    BackendBundle {
        config,
        drop_guards: vec![],
    }
}

pub async fn postgres_memory(smtp_port: u16, http_port: u16) -> BackendBundle {
    let attachment_dir_path =
        std::env::temp_dir().join(format!("catapulte_scenarios_pgmem_{http_port}"));
    let config = AppConfig {
        storage: StorageBackendConfig::Postgres(PostgresConfig {
            url: fresh_database_url(),
            max_connections: 10,
            acquire_timeout: std::time::Duration::from_secs(30),
        }),
        http: InboundHttpConfig {
            address: format!("127.0.0.1:{http_port}").parse().unwrap(),
            auth: None,
            feedback_secret: None,
            request_timeout: std::time::Duration::from_secs(30),
        },
        inbound_nats: None,
        smtp: base_smtp(smtp_port),
        resolver: base_resolver(),
        worker: WorkerConfig::default(),
        queue: QueueBackendConfig::Memory,
        publisher: PublisherAdapterConfig::storage_only(),
        attachment_store: AttachmentStoreBackendConfig::Fs(FsAttachmentStoreConfig {
            root: attachment_dir_path,
        }),
        attachment_fetcher: base_attachment_fetcher(),
        include_loader: catapulte_outbound_mjml::include_loader::IncludeLoaderConfig::default(),
        render: catapulte::render_sandbox::RenderSandboxConfig::new(
            env!("CARGO_BIN_EXE_catapulte").into(),
        ),
        gc_sweep_interval: std::time::Duration::from_hours(1),
        gc_grace_period: std::time::Duration::from_hours(1),
        max_message_bytes: 26_214_400,
        max_schedule_ahead: std::time::Duration::from_hours(720),
        allowed_sender_domains: None,
        bounce: None,
    };
    BackendBundle {
        config,
        drop_guards: vec![],
    }
}

pub async fn postgres_nats(smtp_port: u16, http_port: u16) -> BackendBundle {
    let nats_container = start_nats().await;
    let nats_port = nats_container.get_host_port_ipv4(4222).await.unwrap();
    wait_for_tcp(nats_port, std::time::Duration::from_secs(15)).await;
    let attachment_dir_path =
        std::env::temp_dir().join(format!("catapulte_scenarios_pgnats_{http_port}"));
    let config = AppConfig {
        storage: StorageBackendConfig::Postgres(PostgresConfig {
            url: fresh_database_url(),
            max_connections: 10,
            acquire_timeout: std::time::Duration::from_secs(30),
        }),
        http: InboundHttpConfig {
            address: format!("127.0.0.1:{http_port}").parse().unwrap(),
            auth: None,
            feedback_secret: None,
            request_timeout: std::time::Duration::from_secs(30),
        },
        inbound_nats: None,
        smtp: base_smtp(smtp_port),
        resolver: base_resolver(),
        worker: WorkerConfig::default(),
        queue: QueueBackendConfig::Nats(nats_config(
            format!("nats://127.0.0.1:{nats_port}"),
            http_port,
        )),
        publisher: PublisherAdapterConfig::storage_only(),
        attachment_store: AttachmentStoreBackendConfig::Fs(FsAttachmentStoreConfig {
            root: attachment_dir_path,
        }),
        attachment_fetcher: base_attachment_fetcher(),
        include_loader: catapulte_outbound_mjml::include_loader::IncludeLoaderConfig::default(),
        render: catapulte::render_sandbox::RenderSandboxConfig::new(
            env!("CARGO_BIN_EXE_catapulte").into(),
        ),
        gc_sweep_interval: std::time::Duration::from_hours(1),
        gc_grace_period: std::time::Duration::from_hours(1),
        max_message_bytes: 26_214_400,
        max_schedule_ahead: std::time::Duration::from_hours(720),
        allowed_sender_domains: None,
        bounce: None,
    };
    BackendBundle {
        config,
        drop_guards: vec![Box::new(nats_container)],
    }
}
