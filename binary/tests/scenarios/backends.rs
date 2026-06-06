use std::any::Any;
use std::collections::HashSet;

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
use testcontainers::core::{IntoContainerPort, WaitFor};
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
    }
}

fn base_attachment_fetcher() -> HttpAttachmentFetcherConfig {
    HttpAttachmentFetcherConfig::default()
}

async fn start_postgres() -> testcontainers::ContainerAsync<GenericImage> {
    GenericImage::new("postgres", "16-alpine")
        .with_exposed_port(5432.tcp())
        .with_wait_for(WaitFor::message_on_stderr(
            "database system is ready to accept connections",
        ))
        .with_env_var("POSTGRES_USER", "catapulte")
        .with_env_var("POSTGRES_PASSWORD", "catapulte")
        .with_env_var("POSTGRES_DB", "catapulte")
        .start()
        .await
        .expect("failed to start postgres container; ensure Docker is running")
}

async fn start_nats() -> testcontainers::ContainerAsync<GenericImage> {
    GenericImage::new("nats", "2-alpine")
        .with_exposed_port(4222.tcp())
        .with_wait_for(WaitFor::message_on_stderr("Server is ready"))
        .with_wait_for(WaitFor::millis(200))
        .with_cmd(["nats-server".to_owned(), "--js".to_owned()])
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
        },
        inbound_nats: None,
        smtp: base_smtp(smtp_port),
        resolver: base_resolver(),
        worker: WorkerConfig {},
        queue: QueueBackendConfig::Storage,
        publisher: PublisherAdapterConfig::storage_only(),
        attachment_store: AttachmentStoreBackendConfig::Fs(FsAttachmentStoreConfig {
            root: attachment_dir_path,
        }),
        attachment_fetcher: base_attachment_fetcher(),
        include_loader: catapulte_outbound_mjml::include_loader::IncludeLoaderConfig::default(),
        gc_sweep_interval: std::time::Duration::from_hours(1),
        gc_grace_period: std::time::Duration::from_hours(1),
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
        },
        inbound_nats: None,
        smtp: base_smtp(smtp_port),
        resolver: base_resolver(),
        worker: WorkerConfig {},
        queue: QueueBackendConfig::Memory,
        publisher: PublisherAdapterConfig::storage_only(),
        attachment_store: AttachmentStoreBackendConfig::Fs(FsAttachmentStoreConfig {
            root: attachment_dir_path,
        }),
        attachment_fetcher: base_attachment_fetcher(),
        include_loader: catapulte_outbound_mjml::include_loader::IncludeLoaderConfig::default(),
        gc_sweep_interval: std::time::Duration::from_hours(1),
        gc_grace_period: std::time::Duration::from_hours(1),
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
        },
        inbound_nats: None,
        smtp: base_smtp(smtp_port),
        resolver: base_resolver(),
        worker: WorkerConfig {},
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
        gc_sweep_interval: std::time::Duration::from_hours(1),
        gc_grace_period: std::time::Duration::from_hours(1),
    };
    BackendBundle {
        config,
        drop_guards: vec![Box::new(db_dir), Box::new(nats_container)],
    }
}

pub async fn postgres_storage(smtp_port: u16, http_port: u16) -> BackendBundle {
    let pg = start_postgres().await;
    let pg_port = pg.get_host_port_ipv4(5432).await.unwrap();
    wait_for_tcp(pg_port, std::time::Duration::from_secs(15)).await;
    let attachment_dir_path =
        std::env::temp_dir().join(format!("catapulte_scenarios_pg_{http_port}"));
    let config = AppConfig {
        storage: StorageBackendConfig::Postgres(PostgresConfig {
            url: format!("postgres://catapulte:catapulte@127.0.0.1:{pg_port}/catapulte"),
        }),
        http: InboundHttpConfig {
            address: format!("127.0.0.1:{http_port}").parse().unwrap(),
        },
        inbound_nats: None,
        smtp: base_smtp(smtp_port),
        resolver: base_resolver(),
        worker: WorkerConfig {},
        queue: QueueBackendConfig::Storage,
        publisher: PublisherAdapterConfig::storage_only(),
        attachment_store: AttachmentStoreBackendConfig::Fs(FsAttachmentStoreConfig {
            root: attachment_dir_path,
        }),
        attachment_fetcher: base_attachment_fetcher(),
        include_loader: catapulte_outbound_mjml::include_loader::IncludeLoaderConfig::default(),
        gc_sweep_interval: std::time::Duration::from_hours(1),
        gc_grace_period: std::time::Duration::from_hours(1),
    };
    BackendBundle {
        config,
        drop_guards: vec![Box::new(pg)],
    }
}

pub async fn postgres_memory(smtp_port: u16, http_port: u16) -> BackendBundle {
    let pg = start_postgres().await;
    let pg_port = pg.get_host_port_ipv4(5432).await.unwrap();
    wait_for_tcp(pg_port, std::time::Duration::from_secs(15)).await;
    let attachment_dir_path =
        std::env::temp_dir().join(format!("catapulte_scenarios_pgmem_{http_port}"));
    let config = AppConfig {
        storage: StorageBackendConfig::Postgres(PostgresConfig {
            url: format!("postgres://catapulte:catapulte@127.0.0.1:{pg_port}/catapulte"),
        }),
        http: InboundHttpConfig {
            address: format!("127.0.0.1:{http_port}").parse().unwrap(),
        },
        inbound_nats: None,
        smtp: base_smtp(smtp_port),
        resolver: base_resolver(),
        worker: WorkerConfig {},
        queue: QueueBackendConfig::Memory,
        publisher: PublisherAdapterConfig::storage_only(),
        attachment_store: AttachmentStoreBackendConfig::Fs(FsAttachmentStoreConfig {
            root: attachment_dir_path,
        }),
        attachment_fetcher: base_attachment_fetcher(),
        include_loader: catapulte_outbound_mjml::include_loader::IncludeLoaderConfig::default(),
        gc_sweep_interval: std::time::Duration::from_hours(1),
        gc_grace_period: std::time::Duration::from_hours(1),
    };
    BackendBundle {
        config,
        drop_guards: vec![Box::new(pg)],
    }
}

pub async fn postgres_nats(smtp_port: u16, http_port: u16) -> BackendBundle {
    let pg = start_postgres().await;
    let nats_container = start_nats().await;
    let pg_port = pg.get_host_port_ipv4(5432).await.unwrap();
    let nats_port = nats_container.get_host_port_ipv4(4222).await.unwrap();
    wait_for_tcp(pg_port, std::time::Duration::from_secs(15)).await;
    wait_for_tcp(nats_port, std::time::Duration::from_secs(15)).await;
    let attachment_dir_path =
        std::env::temp_dir().join(format!("catapulte_scenarios_pgnats_{http_port}"));
    let config = AppConfig {
        storage: StorageBackendConfig::Postgres(PostgresConfig {
            url: format!("postgres://catapulte:catapulte@127.0.0.1:{pg_port}/catapulte"),
        }),
        http: InboundHttpConfig {
            address: format!("127.0.0.1:{http_port}").parse().unwrap(),
        },
        inbound_nats: None,
        smtp: base_smtp(smtp_port),
        resolver: base_resolver(),
        worker: WorkerConfig {},
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
        gc_sweep_interval: std::time::Duration::from_hours(1),
        gc_grace_period: std::time::Duration::from_hours(1),
    };
    BackendBundle {
        config,
        drop_guards: vec![Box::new(pg), Box::new(nats_container)],
    }
}
