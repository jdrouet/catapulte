use std::collections::HashSet;
use std::net::TcpListener;
use std::time::Duration;

use catapulte::AppConfig;
use catapulte::queue::QueueBackendConfig;
use catapulte::storage::StorageBackendConfig;
use catapulte_inbound_http::InboundHttpConfig;
use catapulte_inbound_worker::worker::WorkerConfig;
use catapulte_outbound_postgres::PostgresConfig;
use catapulte_outbound_resolver::resolver::TemplateResolverConfig;
use catapulte_outbound_smtp::sender::{SmtpConfig, SmtpTls};
use catapulte_outbound_sqlite::SqliteConfig;
use testcontainers::GenericImage;
use testcontainers::ImageExt;
use testcontainers::core::{IntoContainerPort, WaitFor};
use testcontainers::runners::AsyncRunner;

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
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

fn base_smtp(port: u16) -> SmtpConfig {
    SmtpConfig {
        host: "127.0.0.1".to_owned(),
        port,
        username: None,
        password: None,
        tls: SmtpTls::None,
    }
}

fn base_resolver() -> TemplateResolverConfig {
    TemplateResolverConfig {
        allowed_domains: HashSet::new(),
        templates_dir: None,
    }
}

fn test_nats_config(url: String) -> catapulte_outbound_nats::NatsConfig {
    catapulte_outbound_nats::NatsConfig {
        url,
        stream: "CATAPULTE_E2E".to_owned(),
        subject: "catapulte.emails.queued".to_owned(),
        consumer: "e2e-worker".to_owned(),
        ack_wait_secs: 5,
        max_deliver: 3,
        backoff_secs: vec![1, 2, 3],
    }
}

async fn assert_email_delivered(config: AppConfig, http_port: u16, api_port: u16) {
    let app = config.build().await.expect("failed to build app");
    tokio::spawn(async move {
        let _ = app.run().await;
    });

    let client = reqwest::Client::new();

    for _ in 0..100 {
        if client
            .post(format!("http://127.0.0.1:{http_port}/emails"))
            .body("")
            .send()
            .await
            .is_ok()
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let resp = client
        .post(format!("http://127.0.0.1:{http_port}/emails"))
        .json(&serde_json::json!({
            "sender": "sender@example.com",
            "recipients": [{ "kind": "to", "address": "recipient@example.com" }],
            "body": { "kind": "plain", "text": "Hello from catapulte e2e!" },
            "variables": {}
        }))
        .send()
        .await
        .expect("POST /emails failed");

    assert!(
        resp.status().is_success(),
        "unexpected status: {}",
        resp.status()
    );

    let messages_url = format!("http://127.0.0.1:{api_port}/api/v1/messages");
    let mut delivered = false;
    for _ in 0..100 {
        if let Ok(body) = client
            .get(&messages_url)
            .send()
            .await
            .unwrap()
            .json::<serde_json::Value>()
            .await
            && body["messages"].as_array().is_some_and(|a| !a.is_empty())
        {
            delivered = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    assert!(
        delivered,
        "email was not delivered to mailpit within timeout"
    );
}

#[tokio::test]
async fn submit_plain_email_is_delivered_via_mailpit() {
    let mailpit = start_mailpit().await;
    let smtp_port = mailpit.get_host_port_ipv4(1025).await.unwrap();
    let api_port = mailpit.get_host_port_ipv4(8025).await.unwrap();

    let db_dir = tempfile::tempdir().unwrap();
    let db_path = db_dir.path().join("catapulte_e2e.db");
    let http_port = free_port();

    let config = AppConfig {
        storage: StorageBackendConfig::Sqlite(SqliteConfig {
            url: format!("sqlite:{}", db_path.display()),
        }),
        http: InboundHttpConfig {
            address: format!("127.0.0.1:{http_port}").parse().unwrap(),
        },
        smtp: base_smtp(smtp_port),
        resolver: base_resolver(),
        worker: WorkerConfig {},
        queue: QueueBackendConfig::Storage,
    };

    assert_email_delivered(config, http_port, api_port).await;
}

#[tokio::test]
async fn submit_plain_email_with_memory_queue_is_delivered() {
    let mailpit = start_mailpit().await;
    let smtp_port = mailpit.get_host_port_ipv4(1025).await.unwrap();
    let api_port = mailpit.get_host_port_ipv4(8025).await.unwrap();

    let db_dir = tempfile::tempdir().unwrap();
    let db_path = db_dir.path().join("catapulte_e2e_memory.db");
    let http_port = free_port();

    let config = AppConfig {
        storage: StorageBackendConfig::Sqlite(SqliteConfig {
            url: format!("sqlite:{}", db_path.display()),
        }),
        http: InboundHttpConfig {
            address: format!("127.0.0.1:{http_port}").parse().unwrap(),
        },
        smtp: base_smtp(smtp_port),
        resolver: base_resolver(),
        worker: WorkerConfig {},
        queue: QueueBackendConfig::Memory,
    };

    assert_email_delivered(config, http_port, api_port).await;
}

#[tokio::test]
async fn submit_email_sqlite_storage_nats_queue_is_delivered() {
    let mailpit = start_mailpit().await;
    let nats = start_nats().await;

    let smtp_port = mailpit.get_host_port_ipv4(1025).await.unwrap();
    let api_port = mailpit.get_host_port_ipv4(8025).await.unwrap();
    let nats_port = nats.get_host_port_ipv4(4222).await.unwrap();

    let db_dir = tempfile::tempdir().unwrap();
    let db_path = db_dir.path().join("catapulte_e2e_sqlite_nats.db");
    let http_port = free_port();

    let config = AppConfig {
        storage: StorageBackendConfig::Sqlite(SqliteConfig {
            url: format!("sqlite:{}", db_path.display()),
        }),
        http: InboundHttpConfig {
            address: format!("127.0.0.1:{http_port}").parse().unwrap(),
        },
        smtp: base_smtp(smtp_port),
        resolver: base_resolver(),
        worker: WorkerConfig {},
        queue: QueueBackendConfig::Nats(test_nats_config(format!("nats://127.0.0.1:{nats_port}"))),
    };

    assert_email_delivered(config, http_port, api_port).await;
}

#[tokio::test]
async fn submit_email_postgres_storage_storage_queue_is_delivered() {
    let mailpit = start_mailpit().await;
    let pg = start_postgres().await;

    let smtp_port = mailpit.get_host_port_ipv4(1025).await.unwrap();
    let api_port = mailpit.get_host_port_ipv4(8025).await.unwrap();
    let pg_port = pg.get_host_port_ipv4(5432).await.unwrap();

    let http_port = free_port();

    let config = AppConfig {
        storage: StorageBackendConfig::Postgres(PostgresConfig {
            url: format!("postgres://catapulte:catapulte@127.0.0.1:{pg_port}/catapulte"),
        }),
        http: InboundHttpConfig {
            address: format!("127.0.0.1:{http_port}").parse().unwrap(),
        },
        smtp: base_smtp(smtp_port),
        resolver: base_resolver(),
        worker: WorkerConfig {},
        queue: QueueBackendConfig::Storage,
    };

    assert_email_delivered(config, http_port, api_port).await;
}

#[tokio::test]
async fn submit_email_postgres_storage_memory_queue_is_delivered() {
    let mailpit = start_mailpit().await;
    let pg = start_postgres().await;

    let smtp_port = mailpit.get_host_port_ipv4(1025).await.unwrap();
    let api_port = mailpit.get_host_port_ipv4(8025).await.unwrap();
    let pg_port = pg.get_host_port_ipv4(5432).await.unwrap();

    let http_port = free_port();

    let config = AppConfig {
        storage: StorageBackendConfig::Postgres(PostgresConfig {
            url: format!("postgres://catapulte:catapulte@127.0.0.1:{pg_port}/catapulte"),
        }),
        http: InboundHttpConfig {
            address: format!("127.0.0.1:{http_port}").parse().unwrap(),
        },
        smtp: base_smtp(smtp_port),
        resolver: base_resolver(),
        worker: WorkerConfig {},
        queue: QueueBackendConfig::Memory,
    };

    assert_email_delivered(config, http_port, api_port).await;
}

#[tokio::test]
async fn submit_email_postgres_storage_nats_queue_is_delivered() {
    let mailpit = start_mailpit().await;
    let pg = start_postgres().await;
    let nats = start_nats().await;

    let smtp_port = mailpit.get_host_port_ipv4(1025).await.unwrap();
    let api_port = mailpit.get_host_port_ipv4(8025).await.unwrap();
    let pg_port = pg.get_host_port_ipv4(5432).await.unwrap();
    let nats_port = nats.get_host_port_ipv4(4222).await.unwrap();

    let http_port = free_port();

    let config = AppConfig {
        storage: StorageBackendConfig::Postgres(PostgresConfig {
            url: format!("postgres://catapulte:catapulte@127.0.0.1:{pg_port}/catapulte"),
        }),
        http: InboundHttpConfig {
            address: format!("127.0.0.1:{http_port}").parse().unwrap(),
        },
        smtp: base_smtp(smtp_port),
        resolver: base_resolver(),
        worker: WorkerConfig {},
        queue: QueueBackendConfig::Nats(test_nats_config(format!("nats://127.0.0.1:{nats_port}"))),
    };

    assert_email_delivered(config, http_port, api_port).await;
}
