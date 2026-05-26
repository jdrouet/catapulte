use std::collections::HashSet;
use std::net::TcpListener;
use std::time::Duration;

use catapulte::AppConfig;
use catapulte::attachment_store::AttachmentStoreBackendConfig;
use catapulte::publisher::PublisherAdapterConfig;
use catapulte::queue::QueueBackendConfig;
use catapulte::storage::StorageBackendConfig;
use catapulte_inbound_http::InboundHttpConfig;
use catapulte_inbound_worker::worker::WorkerConfig;
use catapulte_outbound_attachment_fs::store::FsAttachmentStoreConfig;
use catapulte_outbound_postgres::PostgresConfig;
use catapulte_outbound_resolver::resolver::TemplateResolverConfig;
use catapulte_outbound_smtp::multi_sender::MultiSenderConfig;
use catapulte_outbound_smtp::transport::{SmtpConfig, SmtpTls};
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

fn base_attachment_store() -> AttachmentStoreBackendConfig {
    AttachmentStoreBackendConfig::Fs(FsAttachmentStoreConfig {
        root: std::env::temp_dir().join("catapulte_e2e_attachments"),
    })
}

fn base_attachment_fetcher()
-> catapulte_outbound_attachment_fetcher::fetcher::HttpAttachmentFetcherConfig {
    catapulte_outbound_attachment_fetcher::fetcher::HttpAttachmentFetcherConfig::default()
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
        inbound_nats: None,
        smtp: base_smtp(smtp_port),
        resolver: base_resolver(),
        worker: WorkerConfig {},
        queue: QueueBackendConfig::Storage,
        publisher: PublisherAdapterConfig::storage_only(),
        attachment_store: base_attachment_store(),
        attachment_fetcher: base_attachment_fetcher(),
        include_loader: catapulte_outbound_mjml::include_loader::IncludeLoaderConfig::default(),
        gc_sweep_interval: Duration::from_secs(3600),
        gc_grace_period: Duration::from_secs(3600),
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
        inbound_nats: None,
        smtp: base_smtp(smtp_port),
        resolver: base_resolver(),
        worker: WorkerConfig {},
        queue: QueueBackendConfig::Memory,
        publisher: PublisherAdapterConfig::storage_only(),
        attachment_store: base_attachment_store(),
        attachment_fetcher: base_attachment_fetcher(),
        include_loader: catapulte_outbound_mjml::include_loader::IncludeLoaderConfig::default(),
        gc_sweep_interval: Duration::from_secs(3600),
        gc_grace_period: Duration::from_secs(3600),
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
        inbound_nats: None,
        smtp: base_smtp(smtp_port),
        resolver: base_resolver(),
        worker: WorkerConfig {},
        queue: QueueBackendConfig::Nats(test_nats_config(format!("nats://127.0.0.1:{nats_port}"))),
        publisher: PublisherAdapterConfig::storage_only(),
        attachment_store: base_attachment_store(),
        attachment_fetcher: base_attachment_fetcher(),
        include_loader: catapulte_outbound_mjml::include_loader::IncludeLoaderConfig::default(),
        gc_sweep_interval: Duration::from_secs(3600),
        gc_grace_period: Duration::from_secs(3600),
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
        inbound_nats: None,
        smtp: base_smtp(smtp_port),
        resolver: base_resolver(),
        worker: WorkerConfig {},
        queue: QueueBackendConfig::Storage,
        publisher: PublisherAdapterConfig::storage_only(),
        attachment_store: base_attachment_store(),
        attachment_fetcher: base_attachment_fetcher(),
        include_loader: catapulte_outbound_mjml::include_loader::IncludeLoaderConfig::default(),
        gc_sweep_interval: Duration::from_secs(3600),
        gc_grace_period: Duration::from_secs(3600),
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
        inbound_nats: None,
        smtp: base_smtp(smtp_port),
        resolver: base_resolver(),
        worker: WorkerConfig {},
        queue: QueueBackendConfig::Memory,
        publisher: PublisherAdapterConfig::storage_only(),
        attachment_store: base_attachment_store(),
        attachment_fetcher: base_attachment_fetcher(),
        include_loader: catapulte_outbound_mjml::include_loader::IncludeLoaderConfig::default(),
        gc_sweep_interval: Duration::from_secs(3600),
        gc_grace_period: Duration::from_secs(3600),
    };

    assert_email_delivered(config, http_port, api_port).await;
}

#[tokio::test]
async fn lifecycle_events_endpoint_returns_queued_and_sent() {
    let mailpit = start_mailpit().await;
    let smtp_port = mailpit.get_host_port_ipv4(1025).await.unwrap();
    let api_port = mailpit.get_host_port_ipv4(8025).await.unwrap();

    let db_dir = tempfile::tempdir().unwrap();
    let db_path = db_dir.path().join("catapulte_e2e_lifecycle.db");
    let http_port = free_port();

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
        attachment_store: base_attachment_store(),
        attachment_fetcher: base_attachment_fetcher(),
        include_loader: catapulte_outbound_mjml::include_loader::IncludeLoaderConfig::default(),
        gc_sweep_interval: Duration::from_secs(3600),
        gc_grace_period: Duration::from_secs(3600),
    };

    let app = config.build().await.expect("failed to build app");
    tokio::spawn(async move {
        let _ = app.run().await;
    });

    let client = reqwest::Client::new();

    // wait for server to be up
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
            "body": { "kind": "plain", "text": "Hello lifecycle events!" },
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
    let body: serde_json::Value = resp.json().await.unwrap();
    let id = body["id"].as_str().expect("id field missing").to_owned();

    // wait for mailpit to receive the email
    let messages_url = format!("http://127.0.0.1:{api_port}/api/v1/messages");
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
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // poll GET /emails/{id}/events until "sent" appears
    let events_url = format!("http://127.0.0.1:{http_port}/emails/{id}/events");
    let mut found_sent = false;
    for _ in 0..100 {
        if let Ok(resp) = client.get(&events_url).send().await
            && let Ok(body) = resp.json::<serde_json::Value>().await
        {
            let events = body["events"].as_array().cloned().unwrap_or_default();
            if events
                .iter()
                .any(|e| e["event_type"].as_str() == Some("sent"))
            {
                found_sent = true;
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(found_sent, "no 'sent' event found within timeout");

    // verify queued event is also present
    let resp = client.get(&events_url).send().await.unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    let events = body["events"].as_array().expect("events array");
    let has_queued = events
        .iter()
        .any(|e| e["event_type"].as_str() == Some("queued"));
    assert!(has_queued, "no 'queued' event found");
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn list_endpoints_return_submitted_email() {
    let mailpit = start_mailpit().await;
    let smtp_port = mailpit.get_host_port_ipv4(1025).await.unwrap();
    let api_port = mailpit.get_host_port_ipv4(8025).await.unwrap();

    let db_dir = tempfile::tempdir().unwrap();
    let db_path = db_dir.path().join("catapulte_e2e_list.db");
    let http_port = free_port();

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
        attachment_store: base_attachment_store(),
        attachment_fetcher: base_attachment_fetcher(),
        include_loader: catapulte_outbound_mjml::include_loader::IncludeLoaderConfig::default(),
        gc_sweep_interval: Duration::from_secs(3600),
        gc_grace_period: Duration::from_secs(3600),
    };

    let app = config.build().await.expect("failed to build app");
    tokio::spawn(async move {
        let _ = app.run().await;
    });

    let client = reqwest::Client::new();

    // wait for server to be up
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

    // submit an email and capture its id
    let resp = client
        .post(format!("http://127.0.0.1:{http_port}/emails"))
        .json(&serde_json::json!({
            "sender": "sender@example.com",
            "recipients": [{ "kind": "to", "address": "recipient@example.com" }],
            "body": { "kind": "plain", "text": "Hello list endpoints!" },
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
    let body: serde_json::Value = resp.json().await.unwrap();
    let id = body["id"].as_str().expect("id field missing").to_owned();

    // wait for mailpit to receive the email
    let messages_url = format!("http://127.0.0.1:{api_port}/api/v1/messages");
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
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // GET /events -> 200, non-empty
    let resp = client
        .get(format!("http://127.0.0.1:{http_port}/events"))
        .send()
        .await
        .expect("GET /events failed");
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["events"].as_array().is_some_and(|a| !a.is_empty()),
        "expected non-empty events array"
    );

    // GET /events?email_id={id} -> all events have email_id == id
    let resp = client
        .get(format!("http://127.0.0.1:{http_port}/events?email_id={id}"))
        .send()
        .await
        .expect("GET /events?email_id failed");
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let events = body["events"].as_array().expect("events array");
    for event in events {
        assert_eq!(
            event["email_id"].as_str(),
            Some(id.as_str()),
            "event email_id mismatch"
        );
    }

    // GET /emails -> 200, contains entry with id == submitted id
    let resp = client
        .get(format!("http://127.0.0.1:{http_port}/emails"))
        .send()
        .await
        .expect("GET /emails failed");
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let emails = body["emails"].as_array().expect("emails array");
    assert!(
        emails.iter().any(|e| e["id"].as_str() == Some(id.as_str())),
        "submitted email id not found in GET /emails response"
    );

    // GET /emails?id={id} -> exactly one entry
    let resp = client
        .get(format!("http://127.0.0.1:{http_port}/emails?id={id}"))
        .send()
        .await
        .expect("GET /emails?id failed");
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let emails = body["emails"].as_array().expect("emails array");
    assert_eq!(emails.len(), 1, "expected exactly one email for id filter");
    assert_eq!(emails[0]["id"].as_str(), Some(id.as_str()));
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
        inbound_nats: None,
        smtp: base_smtp(smtp_port),
        resolver: base_resolver(),
        worker: WorkerConfig {},
        queue: QueueBackendConfig::Nats(test_nats_config(format!("nats://127.0.0.1:{nats_port}"))),
        publisher: PublisherAdapterConfig::storage_only(),
        attachment_store: base_attachment_store(),
        attachment_fetcher: base_attachment_fetcher(),
        include_loader: catapulte_outbound_mjml::include_loader::IncludeLoaderConfig::default(),
        gc_sweep_interval: Duration::from_secs(3600),
        gc_grace_period: Duration::from_secs(3600),
    };

    assert_email_delivered(config, http_port, api_port).await;
}

#[tokio::test]
async fn multi_sender_primary_delivers_email_before_backup() {
    let mailpit = start_mailpit().await;
    let smtp_port = mailpit.get_host_port_ipv4(1025).await.unwrap();
    let api_port = mailpit.get_host_port_ipv4(8025).await.unwrap();

    let db_dir = tempfile::tempdir().unwrap();
    let db_path = db_dir.path().join("catapulte_e2e_multi_primary.db");
    let http_port = free_port();

    let smtp = MultiSenderConfig::empty()
        .with_sender(
            "primary",
            SmtpConfig {
                host: "127.0.0.1".to_owned(),
                port: smtp_port,
                username: None,
                password: None,
                tls: SmtpTls::None,
            },
            1,
            None,
        )
        .with_sender(
            "backup",
            SmtpConfig {
                host: "127.0.0.1".to_owned(),
                port: smtp_port,
                username: None,
                password: None,
                tls: SmtpTls::None,
            },
            2,
            None,
        );

    let config = AppConfig {
        storage: StorageBackendConfig::Sqlite(SqliteConfig {
            url: format!("sqlite:{}", db_path.display()),
        }),
        http: InboundHttpConfig {
            address: format!("127.0.0.1:{http_port}").parse().unwrap(),
        },
        inbound_nats: None,
        smtp,
        resolver: base_resolver(),
        worker: WorkerConfig {},
        queue: QueueBackendConfig::Storage,
        publisher: PublisherAdapterConfig::storage_only(),
        attachment_store: base_attachment_store(),
        attachment_fetcher: base_attachment_fetcher(),
        include_loader: catapulte_outbound_mjml::include_loader::IncludeLoaderConfig::default(),
        gc_sweep_interval: Duration::from_secs(3600),
        gc_grace_period: Duration::from_secs(3600),
    };

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
            "body": { "kind": "plain", "text": "Hello multi-sender primary!" },
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
    let body: serde_json::Value = resp.json().await.unwrap();
    let id = body["id"].as_str().expect("id field missing").to_owned();

    // wait for mailpit to receive the email
    let messages_url = format!("http://127.0.0.1:{api_port}/api/v1/messages");
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
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // poll GET /emails/{id}/events until "sent" appears
    let events_url = format!("http://127.0.0.1:{http_port}/emails/{id}/events");
    let mut found_sent = false;
    for _ in 0..100 {
        if let Ok(resp) = client.get(&events_url).send().await
            && let Ok(body) = resp.json::<serde_json::Value>().await
        {
            let events = body["events"].as_array().cloned().unwrap_or_default();
            if events
                .iter()
                .any(|e| e["event_type"].as_str() == Some("sent"))
            {
                found_sent = true;
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(found_sent, "no 'sent' event found within timeout");

    let resp = client
        .get(format!("http://127.0.0.1:{http_port}/senders"))
        .send()
        .await
        .expect("GET /senders failed");
    assert_eq!(resp.status(), 200, "GET /senders returned non-200");

    let body: serde_json::Value = resp.json().await.unwrap();
    let senders = body["senders"].as_array().expect("senders array");

    let primary = senders
        .iter()
        .find(|s| s["name"].as_str() == Some("primary"))
        .expect("primary sender not found in /senders response");
    let backup = senders
        .iter()
        .find(|s| s["name"].as_str() == Some("backup"))
        .expect("backup sender not found in /senders response");

    assert_eq!(
        primary["sent_in_range"].as_u64(),
        Some(1),
        "primary should have sent_in_range == 1"
    );
    assert_eq!(
        backup["sent_in_range"].as_u64(),
        Some(0),
        "backup should have sent_in_range == 0"
    );
}

#[tokio::test]
async fn submit_mjml_inline_with_variables_renders_and_delivers() {
    let mailpit = start_mailpit().await;
    let smtp_port = mailpit.get_host_port_ipv4(1025).await.unwrap();
    let api_port = mailpit.get_host_port_ipv4(8025).await.unwrap();

    let db_dir = tempfile::tempdir().unwrap();
    let db_path = db_dir.path().join("catapulte_e2e_mjml_vars.db");
    let http_port = free_port();

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
        attachment_store: base_attachment_store(),
        attachment_fetcher: base_attachment_fetcher(),
        include_loader: catapulte_outbound_mjml::include_loader::IncludeLoaderConfig::default(),
        gc_sweep_interval: Duration::from_secs(3600),
        gc_grace_period: Duration::from_secs(3600),
    };

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

    let mjml_source = r"<mjml>
  <mj-head>
    <mj-preview>Hello {{ name }}!</mj-preview>
  </mj-head>
  <mj-body>
    <mj-section>
      <mj-column>
        <mj-text>Hello {{ name }}!</mj-text>
      </mj-column>
    </mj-section>
  </mj-body>
</mjml>";

    let resp = client
        .post(format!("http://127.0.0.1:{http_port}/emails"))
        .json(&serde_json::json!({
            "sender": "sender@example.com",
            "recipients": [{ "kind": "to", "address": "recipient@example.com" }],
            "body": { "kind": "mjml_inline", "source": mjml_source },
            "variables": { "name": "World" }
        }))
        .send()
        .await
        .expect("POST /emails failed");
    assert!(
        resp.status().is_success(),
        "unexpected status: {}",
        resp.status()
    );

    // wait for mailpit to receive the email and grab its ID
    let messages_url = format!("http://127.0.0.1:{api_port}/api/v1/messages");
    let mut message_id: Option<String> = None;
    for _ in 0..100 {
        if let Ok(body) = client
            .get(&messages_url)
            .send()
            .await
            .unwrap()
            .json::<serde_json::Value>()
            .await
            && let Some(msgs) = body["messages"].as_array()
            && let Some(first) = msgs.first()
        {
            message_id = first["ID"].as_str().map(str::to_owned);
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let msg_id = message_id.expect("email was not delivered to mailpit within timeout");

    let msg: serde_json::Value = client
        .get(format!(
            "http://127.0.0.1:{api_port}/api/v1/message/{msg_id}"
        ))
        .send()
        .await
        .expect("GET /api/v1/message/{id} failed")
        .json()
        .await
        .unwrap();

    let html = msg["HTML"].as_str().unwrap_or("");
    assert!(
        html.contains("Hello World!"),
        "expected rendered HTML to contain 'Hello World!', got: {html}"
    );
    let text = msg["Text"].as_str().unwrap_or("");
    assert!(
        text.contains("Hello World!"),
        "expected text part (from mj-preview) to contain 'Hello World!', got: {text}"
    );
}

#[tokio::test]
async fn idempotency_key_deduplicates_submission() {
    let mailpit = start_mailpit().await;
    let smtp_port = mailpit.get_host_port_ipv4(1025).await.unwrap();
    let api_port = mailpit.get_host_port_ipv4(8025).await.unwrap();

    let db_dir = tempfile::tempdir().unwrap();
    let db_path = db_dir.path().join("catapulte_e2e_idempotency.db");
    let http_port = free_port();

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
        attachment_store: base_attachment_store(),
        attachment_fetcher: base_attachment_fetcher(),
        include_loader: catapulte_outbound_mjml::include_loader::IncludeLoaderConfig::default(),
        gc_sweep_interval: Duration::from_secs(3600),
        gc_grace_period: Duration::from_secs(3600),
    };

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

    let payload = serde_json::json!({
        "idempotency_key": "e2e-idempotency-key",
        "sender": "sender@example.com",
        "recipients": [{ "kind": "to", "address": "recipient@example.com" }],
        "body": { "kind": "plain", "text": "Idempotency test." },
        "variables": {}
    });

    let resp1 = client
        .post(format!("http://127.0.0.1:{http_port}/emails"))
        .json(&payload)
        .send()
        .await
        .expect("first POST /emails failed");
    assert!(
        resp1.status().is_success(),
        "first request failed: {}",
        resp1.status()
    );
    let id1 = resp1.json::<serde_json::Value>().await.unwrap()["id"]
        .as_str()
        .expect("id missing from first response")
        .to_owned();

    let resp2 = client
        .post(format!("http://127.0.0.1:{http_port}/emails"))
        .json(&payload)
        .send()
        .await
        .expect("second POST /emails failed");
    assert!(
        resp2.status().is_success(),
        "second request failed: {}",
        resp2.status()
    );
    let id2 = resp2.json::<serde_json::Value>().await.unwrap()["id"]
        .as_str()
        .expect("id missing from second response")
        .to_owned();

    assert_eq!(id1, id2, "both requests must return the same email id");

    // wait for delivery and verify exactly one email arrived
    let messages_url = format!("http://127.0.0.1:{api_port}/api/v1/messages");
    let mut delivered_count: usize = 0;
    for _ in 0..100 {
        if let Ok(body) = client
            .get(&messages_url)
            .send()
            .await
            .unwrap()
            .json::<serde_json::Value>()
            .await
            && let Some(msgs) = body["messages"].as_array()
            && !msgs.is_empty()
        {
            delivered_count = msgs.len();
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    assert_eq!(
        delivered_count, 1,
        "expected exactly one delivered email, got {delivered_count}"
    );
}

#[tokio::test]
async fn multi_sender_falls_back_to_backup_when_primary_fails() {
    let mailpit = start_mailpit().await;
    let smtp_port = mailpit.get_host_port_ipv4(1025).await.unwrap();
    let api_port = mailpit.get_host_port_ipv4(8025).await.unwrap();

    let db_dir = tempfile::tempdir().unwrap();
    let db_path = db_dir.path().join("catapulte_e2e_multi_fallback.db");
    let http_port = free_port();

    let smtp = MultiSenderConfig::empty()
        .with_sender(
            "primary",
            SmtpConfig {
                host: "127.0.0.1".to_owned(),
                port: 1,
                username: None,
                password: None,
                tls: SmtpTls::None,
            },
            1,
            None,
        )
        .with_sender(
            "backup",
            SmtpConfig {
                host: "127.0.0.1".to_owned(),
                port: smtp_port,
                username: None,
                password: None,
                tls: SmtpTls::None,
            },
            2,
            None,
        );

    let config = AppConfig {
        storage: StorageBackendConfig::Sqlite(SqliteConfig {
            url: format!("sqlite:{}", db_path.display()),
        }),
        http: InboundHttpConfig {
            address: format!("127.0.0.1:{http_port}").parse().unwrap(),
        },
        inbound_nats: None,
        smtp,
        resolver: base_resolver(),
        worker: WorkerConfig {},
        queue: QueueBackendConfig::Storage,
        publisher: PublisherAdapterConfig::storage_only(),
        attachment_store: base_attachment_store(),
        attachment_fetcher: base_attachment_fetcher(),
        include_loader: catapulte_outbound_mjml::include_loader::IncludeLoaderConfig::default(),
        gc_sweep_interval: Duration::from_secs(3600),
        gc_grace_period: Duration::from_secs(3600),
    };

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
            "body": { "kind": "plain", "text": "Hello multi-sender fallback!" },
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
    let body: serde_json::Value = resp.json().await.unwrap();
    let id = body["id"].as_str().expect("id field missing").to_owned();

    // wait for mailpit to receive the email
    let messages_url = format!("http://127.0.0.1:{api_port}/api/v1/messages");
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
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // poll GET /emails/{id}/events until "sent" appears
    let events_url = format!("http://127.0.0.1:{http_port}/emails/{id}/events");
    let mut found_sent = false;
    for _ in 0..100 {
        if let Ok(resp) = client.get(&events_url).send().await
            && let Ok(body) = resp.json::<serde_json::Value>().await
        {
            let events = body["events"].as_array().cloned().unwrap_or_default();
            if events
                .iter()
                .any(|e| e["event_type"].as_str() == Some("sent"))
            {
                found_sent = true;
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(found_sent, "no 'sent' event found within timeout");

    let resp = client
        .get(format!("http://127.0.0.1:{http_port}/senders"))
        .send()
        .await
        .expect("GET /senders failed");
    assert_eq!(resp.status(), 200, "GET /senders returned non-200");

    let body: serde_json::Value = resp.json().await.unwrap();
    let senders = body["senders"].as_array().expect("senders array");

    let primary = senders
        .iter()
        .find(|s| s["name"].as_str() == Some("primary"))
        .expect("primary sender not found in /senders response");
    let backup = senders
        .iter()
        .find(|s| s["name"].as_str() == Some("backup"))
        .expect("backup sender not found in /senders response");

    assert_eq!(
        primary["sent_in_range"].as_u64(),
        Some(0),
        "primary should have sent_in_range == 0 (it failed)"
    );
    assert_eq!(
        backup["sent_in_range"].as_u64(),
        Some(1),
        "backup should have sent_in_range == 1 (it handled the fallback)"
    );
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn submit_email_with_inline_attachment_is_delivered_with_attachment() {
    use base64::Engine as _;

    let mailpit = start_mailpit().await;
    let smtp_port = mailpit.get_host_port_ipv4(1025).await.unwrap();
    let api_port = mailpit.get_host_port_ipv4(8025).await.unwrap();

    let db_dir = tempfile::tempdir().unwrap();
    let db_path = db_dir.path().join("catapulte_e2e_attachment.db");
    let http_port = free_port();

    // Keep the tempdir alive for the duration of the test so blobs are readable.
    let attachment_dir = tempfile::tempdir().unwrap();

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
            root: attachment_dir.path().to_path_buf(),
        }),
        attachment_fetcher: base_attachment_fetcher(),
        include_loader: catapulte_outbound_mjml::include_loader::IncludeLoaderConfig::default(),
        gc_sweep_interval: Duration::from_secs(3600),
        gc_grace_period: Duration::from_secs(3600),
    };

    let app = config.build().await.expect("failed to build app");
    tokio::spawn(async move {
        let _ = app.run().await;
    });

    let client = reqwest::Client::new();

    // Wait for server to be up.
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

    let attachment_content = b"Hello attachment";
    let inline_base64 = base64::engine::general_purpose::STANDARD.encode(attachment_content);

    let resp = client
        .post(format!("http://127.0.0.1:{http_port}/emails"))
        .json(&serde_json::json!({
            "sender": "sender@example.com",
            "recipients": [{ "kind": "to", "address": "recipient@example.com" }],
            "body": { "kind": "plain", "text": "Email with attachment" },
            "variables": {},
            "attachments": [{
                "filename": "test.txt",
                "content_type": "text/plain",
                "inline_base64": inline_base64
            }]
        }))
        .send()
        .await
        .expect("POST /emails failed");

    assert!(
        resp.status().is_success(),
        "unexpected status: {}",
        resp.status()
    );

    // Poll mailpit until the message arrives.
    let messages_url = format!("http://127.0.0.1:{api_port}/api/v1/messages");
    let mut message_id: Option<String> = None;
    for _ in 0..100 {
        if let Ok(body) = client
            .get(&messages_url)
            .send()
            .await
            .unwrap()
            .json::<serde_json::Value>()
            .await
            && let Some(msgs) = body["messages"].as_array()
            && let Some(first) = msgs.first()
        {
            message_id = first["ID"].as_str().map(str::to_owned);
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let msg_id = message_id.expect("email was not delivered to mailpit within timeout");

    let msg: serde_json::Value = client
        .get(format!(
            "http://127.0.0.1:{api_port}/api/v1/message/{msg_id}"
        ))
        .send()
        .await
        .expect("GET /api/v1/message/{id} failed")
        .json()
        .await
        .unwrap();

    let attachments = msg["Attachments"].as_array().expect("Attachments array");
    assert_eq!(attachments.len(), 1, "expected exactly one attachment");

    let att = &attachments[0];
    assert_eq!(
        att["FileName"].as_str(),
        Some("test.txt"),
        "attachment filename mismatch"
    );
    let content_type = att["ContentType"].as_str().unwrap_or("");
    assert!(
        content_type.starts_with("text/plain"),
        "expected content type to start with text/plain, got: {content_type}"
    );

    // Keep attachment_dir alive until this point.
    drop(attachment_dir);
}

#[tokio::test]
async fn sent_email_blob_is_deleted_after_delivery() {
    use base64::Engine as _;

    let mailpit = start_mailpit().await;
    let smtp_port = mailpit.get_host_port_ipv4(1025).await.unwrap();
    let api_port = mailpit.get_host_port_ipv4(8025).await.unwrap();

    let db_dir = tempfile::tempdir().unwrap();
    let db_path = db_dir.path().join("catapulte_e2e_blob_cleanup.db");
    let http_port = free_port();
    let attachment_dir = tempfile::tempdir().unwrap();

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
            root: attachment_dir.path().to_path_buf(),
        }),
        attachment_fetcher: base_attachment_fetcher(),
        include_loader: catapulte_outbound_mjml::include_loader::IncludeLoaderConfig::default(),
        gc_sweep_interval: Duration::from_secs(3600),
        gc_grace_period: Duration::from_secs(3600),
    };

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

    let attachment_content = b"blob cleanup test content";
    let inline_base64 = base64::engine::general_purpose::STANDARD.encode(attachment_content);

    let resp = client
        .post(format!("http://127.0.0.1:{http_port}/emails"))
        .json(&serde_json::json!({
            "sender": "sender@example.com",
            "recipients": [{ "kind": "to", "address": "recipient@example.com" }],
            "body": { "kind": "plain", "text": "blob cleanup test" },
            "variables": {},
            "attachments": [{
                "filename": "cleanup.txt",
                "content_type": "text/plain",
                "inline_base64": inline_base64
            }]
        }))
        .send()
        .await
        .expect("POST /emails failed");
    assert!(resp.status().is_success(), "POST failed: {}", resp.status());

    // Wait for delivery.
    let messages_url = format!("http://127.0.0.1:{api_port}/api/v1/messages");
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
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // Poll until no blob files remain or timeout.
    let attachment_path = attachment_dir.path().to_path_buf();
    let mut blobs_gone = false;
    for _ in 0..50 {
        let count = std::fs::read_dir(&attachment_path)
            .expect("read attachment dir")
            .filter_map(|e| e.ok())
            .filter(|e| {
                let name = e.file_name();
                let n = name.to_string_lossy();
                !n.starts_with('.') && e.file_type().map(|ft| ft.is_file()).unwrap_or(false)
            })
            .count();
        if count == 0 {
            blobs_gone = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    assert!(
        blobs_gone,
        "blob file should have been deleted after delivery"
    );
    drop(attachment_dir);
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn submit_email_with_remote_url_attachment_is_delivered() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let mailpit = start_mailpit().await;
    let smtp_port = mailpit.get_host_port_ipv4(1025).await.unwrap();
    let api_port = mailpit.get_host_port_ipv4(8025).await.unwrap();

    let db_dir = tempfile::tempdir().unwrap();
    let db_path = db_dir.path().join("catapulte_e2e_remote_att.db");
    let http_port = free_port();
    let attachment_dir = tempfile::tempdir().unwrap();

    // Start a wiremock server to serve the remote attachment.
    let mock_server = MockServer::start().await;
    let remote_content = b"remote attachment content";
    Mock::given(method("GET"))
        .and(path("/file.pdf"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(remote_content.to_vec())
                .append_header("Content-Type", "application/pdf")
                .append_header("Content-Length", remote_content.len().to_string().as_str()),
        )
        .mount(&mock_server)
        .await;

    let mock_host = mock_server.address().ip().to_string();
    let mock_port = mock_server.address().port();

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
            root: attachment_dir.path().to_path_buf(),
        }),
        attachment_fetcher:
            catapulte_outbound_attachment_fetcher::fetcher::HttpAttachmentFetcherConfig {
                allowed_domains: std::collections::HashSet::from([mock_host.clone()]),
                allow_http: true,
                max_bytes: 25 * 1024 * 1024,
                fetch_timeout: Duration::from_secs(30),
            },
        include_loader: catapulte_outbound_mjml::include_loader::IncludeLoaderConfig::default(),
        gc_sweep_interval: Duration::from_secs(3600),
        gc_grace_period: Duration::from_secs(3600),
    };

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

    let attachment_url = format!("http://{mock_host}:{mock_port}/file.pdf");
    let resp = client
        .post(format!("http://127.0.0.1:{http_port}/emails"))
        .json(&serde_json::json!({
            "sender": "sender@example.com",
            "recipients": [{ "kind": "to", "address": "recipient@example.com" }],
            "body": { "kind": "plain", "text": "remote attachment test" },
            "variables": {},
            "attachments": [{
                "filename": "file.pdf",
                "content_type": "application/pdf",
                "url": attachment_url
            }]
        }))
        .send()
        .await
        .expect("POST /emails failed");

    assert!(resp.status().is_success(), "POST failed: {}", resp.status());

    // Poll mailpit until the message arrives.
    let messages_url = format!("http://127.0.0.1:{api_port}/api/v1/messages");
    let mut message_id: Option<String> = None;
    for _ in 0..100 {
        if let Ok(body) = client
            .get(&messages_url)
            .send()
            .await
            .unwrap()
            .json::<serde_json::Value>()
            .await
            && let Some(msgs) = body["messages"].as_array()
            && let Some(first) = msgs.first()
        {
            message_id = first["ID"].as_str().map(str::to_owned);
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let msg_id = message_id.expect("email was not delivered to mailpit within timeout");

    let msg: serde_json::Value = client
        .get(format!(
            "http://127.0.0.1:{api_port}/api/v1/message/{msg_id}"
        ))
        .send()
        .await
        .expect("GET /api/v1/message/{id} failed")
        .json()
        .await
        .unwrap();

    let attachments = msg["Attachments"].as_array().expect("Attachments array");
    assert_eq!(attachments.len(), 1, "expected exactly one attachment");

    let att = &attachments[0];
    assert_eq!(
        att["FileName"].as_str(),
        Some("file.pdf"),
        "attachment filename mismatch"
    );
    let content_type = att["ContentType"].as_str().unwrap_or("");
    assert!(
        content_type.starts_with("application/pdf"),
        "expected content type to start with application/pdf, got: {content_type}"
    );

    drop(attachment_dir);
}

#[tokio::test]
async fn batch_submit_delivers_multiple_emails() {
    let mailpit = start_mailpit().await;
    let smtp_port = mailpit.get_host_port_ipv4(1025).await.unwrap();
    let api_port = mailpit.get_host_port_ipv4(8025).await.unwrap();

    let db_dir = tempfile::tempdir().unwrap();
    let db_path = db_dir.path().join("catapulte_e2e_batch.db");
    let http_port = free_port();
    let attachment_dir = tempfile::tempdir().unwrap();

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
            root: attachment_dir.path().to_path_buf(),
        }),
        attachment_fetcher: base_attachment_fetcher(),
        include_loader: catapulte_outbound_mjml::include_loader::IncludeLoaderConfig::default(),
        gc_sweep_interval: Duration::from_secs(3600),
        gc_grace_period: Duration::from_secs(3600),
    };

    let app = config.build().await.expect("failed to build app");
    tokio::spawn(async move {
        let _ = app.run().await;
    });

    let client = reqwest::Client::new();

    // Wait for server to be up.
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
        .post(format!("http://127.0.0.1:{http_port}/emails/batch"))
        .json(&serde_json::json!({
            "emails": [
                {
                    "sender": "sender@example.com",
                    "recipients": [{ "kind": "to", "address": "alice@example.com" }],
                    "body": { "kind": "plain", "text": "batch email 1" }
                },
                {
                    "sender": "sender@example.com",
                    "recipients": [{ "kind": "to", "address": "bob@example.com" }],
                    "body": { "kind": "plain", "text": "batch email 2" }
                },
                {
                    "sender": "sender@example.com",
                    "recipients": [{ "kind": "to", "address": "carol@example.com" }],
                    "body": { "kind": "plain", "text": "batch email 3" }
                }
            ]
        }))
        .send()
        .await
        .expect("POST /emails/batch failed");

    assert!(
        resp.status().is_success(),
        "unexpected status: {}",
        resp.status()
    );

    let body: serde_json::Value = resp.json().await.unwrap();
    let results = body["results"].as_array().expect("results array");
    assert_eq!(results.len(), 3, "expected 3 results");

    let mut seen_ids = std::collections::HashSet::new();
    for result in results {
        assert_eq!(
            result["status"].as_str(),
            Some("accepted"),
            "all results should be accepted"
        );
        let id = result["id"].as_str().expect("id field");
        uuid::Uuid::parse_str(id).expect("id should be a valid UUID");
        assert!(seen_ids.insert(id.to_owned()), "UUIDs should be distinct");
    }

    // Poll mailpit until all 3 messages arrive.
    let messages_url = format!("http://127.0.0.1:{api_port}/api/v1/messages");
    let mut delivered_count: usize = 0;
    for _ in 0..100 {
        if let Ok(body) = client
            .get(&messages_url)
            .send()
            .await
            .unwrap()
            .json::<serde_json::Value>()
            .await
            && let Some(msgs) = body["messages"].as_array()
            && msgs.len() >= 3
        {
            delivered_count = msgs.len();
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    assert_eq!(
        delivered_count, 3,
        "expected 3 messages delivered to mailpit, got {delivered_count}"
    );

    drop(attachment_dir);
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn submit_via_inbound_nats_delivers_and_emits_lifecycle_event_with_correlation_id() {
    use futures_util::StreamExt as _;

    let nats = start_nats().await;
    let mailpit = start_mailpit().await;

    let nats_port = nats.get_host_port_ipv4(4222).await.unwrap();
    let smtp_port = mailpit.get_host_port_ipv4(1025).await.unwrap();
    let api_port = mailpit.get_host_port_ipv4(8025).await.unwrap();

    let nats_url = format!("nats://127.0.0.1:{nats_port}");

    let db_dir = tempfile::tempdir().unwrap();
    let db_path = db_dir.path().join("catapulte_e2e_inbound_nats.db");
    let http_port = free_port();

    // Subscribe to lifecycle events BEFORE the app starts so no events are missed.
    // NatsEventPublisher uses core NATS publish, so a simple subscribe works.
    let lifecycle_client = async_nats::connect(&nats_url)
        .await
        .expect("failed to connect lifecycle NATS client");
    let mut lifecycle_sub = lifecycle_client
        .subscribe("catapulte.lifecycle")
        .await
        .expect("failed to subscribe to lifecycle subject");

    let config = AppConfig {
        storage: StorageBackendConfig::Sqlite(SqliteConfig {
            url: format!("sqlite:{}", db_path.display()),
        }),
        http: InboundHttpConfig {
            address: format!("127.0.0.1:{http_port}").parse().unwrap(),
        },
        inbound_nats: Some(catapulte_inbound_nats::server::InboundNatsConfig {
            url: nats_url.clone(),
            stream: "CATAPULTE_E2E_IN".to_owned(),
            subject: "catapulte.submissions".to_owned(),
            consumer: "e2e-inbound".to_owned(),
            ack_wait_secs: 5,
            max_deliver: 3,
            backoff_secs: vec![1, 2, 3],
        }),
        smtp: base_smtp(smtp_port),
        resolver: base_resolver(),
        worker: WorkerConfig {},
        queue: QueueBackendConfig::Storage,
        publisher: PublisherAdapterConfig::with_nats_events(
            nats_url.clone(),
            "catapulte.lifecycle".to_owned(),
        ),
        attachment_store: base_attachment_store(),
        attachment_fetcher: base_attachment_fetcher(),
        include_loader: catapulte_outbound_mjml::include_loader::IncludeLoaderConfig::default(),
        gc_sweep_interval: Duration::from_secs(3600),
        gc_grace_period: Duration::from_secs(3600),
    };

    let app = config.build().await.expect("failed to build app");
    tokio::spawn(async move {
        let _ = app.run().await;
    });

    // Wait for the HTTP server to be ready.
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

    // Publish a submission to the inbound NATS subject via JetStream.
    let publisher_client = async_nats::connect(&nats_url)
        .await
        .expect("failed to connect publisher NATS client");
    let js = async_nats::jetstream::new(publisher_client);

    // The inbound NATS server creates the stream on startup; publish directly to the subject.
    let payload = serde_json::json!({
        "correlation_id": "corr-e2e-test",
        "sender": "sender@example.com",
        "recipients": [{ "kind": "to", "address": "recipient@example.com" }],
        "body": { "kind": "plain", "text": "Hello from inbound NATS e2e!" },
        "variables": {}
    });
    js.publish(
        "catapulte.submissions",
        serde_json::to_vec(&payload).unwrap().into(),
    )
    .await
    .expect("failed to publish submission to NATS")
    .await
    .expect("failed to get ack from NATS");

    // Poll mailpit until the email is delivered.
    let messages_url = format!("http://127.0.0.1:{api_port}/api/v1/messages");
    let mut delivered = false;
    for _ in 0..200 {
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

    // Collect lifecycle events and look for a "sent" event with the expected correlation_id.
    let mut found_sent = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, lifecycle_sub.next()).await {
            Ok(Some(msg)) => {
                if let Ok(body) = serde_json::from_slice::<serde_json::Value>(&msg.payload) {
                    if body["event_type"].as_str() == Some("sent")
                        && body["payload"]["correlation_id"].as_str() == Some("corr-e2e-test")
                    {
                        found_sent = true;
                        break;
                    }
                }
            }
            Ok(None) | Err(_) => break,
        }
    }
    assert!(
        found_sent,
        "no 'sent' lifecycle event with correlation_id 'corr-e2e-test' received within timeout"
    );
}

#[tokio::test]
async fn submit_email_with_disallowed_remote_attachment_returns_400() {
    let db_dir = tempfile::tempdir().unwrap();
    let db_path = db_dir.path().join("catapulte_e2e_disallowed_att.db");
    let http_port = free_port();
    let attachment_dir = tempfile::tempdir().unwrap();

    // Default fetcher config has no allowed domains.
    let config = AppConfig {
        storage: StorageBackendConfig::Sqlite(SqliteConfig {
            url: format!("sqlite:{}", db_path.display()),
        }),
        http: InboundHttpConfig {
            address: format!("127.0.0.1:{http_port}").parse().unwrap(),
        },
        inbound_nats: None,
        smtp: base_smtp(1025),
        resolver: base_resolver(),
        worker: WorkerConfig {},
        queue: QueueBackendConfig::Storage,
        publisher: PublisherAdapterConfig::storage_only(),
        attachment_store: AttachmentStoreBackendConfig::Fs(FsAttachmentStoreConfig {
            root: attachment_dir.path().to_path_buf(),
        }),
        attachment_fetcher: base_attachment_fetcher(),
        include_loader: catapulte_outbound_mjml::include_loader::IncludeLoaderConfig::default(),
        gc_sweep_interval: Duration::from_secs(3600),
        gc_grace_period: Duration::from_secs(3600),
    };

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
            "body": { "kind": "plain", "text": "disallowed attachment test" },
            "variables": {},
            "attachments": [{
                "filename": "file.pdf",
                "content_type": "application/pdf",
                "url": "https://example.com/file.pdf"
            }]
        }))
        .send()
        .await
        .expect("POST /emails failed");

    assert_eq!(
        resp.status().as_u16(),
        400,
        "expected 400 for disallowed domain attachment, got: {}",
        resp.status()
    );

    drop(attachment_dir);
}
