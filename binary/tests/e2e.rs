use std::collections::HashSet;
use std::net::TcpListener;
use std::time::Duration;

use catapulte::AppConfig;
use catapulte::attachment_store::AttachmentStoreBackendConfig;
use catapulte::publisher::PublisherAdapterConfig;
use catapulte::queue::QueueBackendConfig;
use catapulte::storage::StorageBackendConfig;
use catapulte_inbound_http::InboundHttpConfig;
use catapulte_inbound_http::auth::{AuthConfig, NamedApiKey};
use catapulte_inbound_worker::worker::WorkerConfig;
use catapulte_outbound_attachment_fs::store::FsAttachmentStoreConfig;
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

/// Polls `127.0.0.1:<port>` until it accepts a TCP connection or the deadline
/// elapses. Mailpit logs its HTTP listener as ready before the Docker
/// host-port proxy reliably accepts connections on the mapped ports, so a real
/// TCP probe is necessary before the worker or test can use either port.
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

/// Starts mailpit, resolves both mapped ports, and probes each with a real TCP
/// connection before returning. Returns `(container, smtp_port, api_port)`.
/// The caller must keep the returned container alive for the duration of the
/// test; dropping it stops the container.
async fn start_mailpit_ready() -> (testcontainers::ContainerAsync<GenericImage>, u16, u16) {
    let mailpit = start_mailpit().await;
    let smtp_port = mailpit.get_host_port_ipv4(1025).await.unwrap();
    let api_port = mailpit.get_host_port_ipv4(8025).await.unwrap();
    wait_for_tcp(smtp_port, Duration::from_secs(15)).await;
    wait_for_tcp(api_port, Duration::from_secs(15)).await;
    (mailpit, smtp_port, api_port)
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
        auth_entries: Vec::new(),
        fetch_timeout: std::time::Duration::from_secs(30),
        max_bytes: 1_048_576,
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

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn multi_sender_primary_delivers_email_before_backup() {
    let (_mailpit, smtp_port, api_port) = start_mailpit_ready().await;

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
            auth: None,
            feedback_secret: None,
            request_timeout: std::time::Duration::from_secs(30),
        },
        inbound_nats: None,
        smtp,
        resolver: base_resolver(),
        worker: WorkerConfig::default(),
        queue: QueueBackendConfig::Storage,
        publisher: PublisherAdapterConfig::storage_only(),
        attachment_store: base_attachment_store(),
        attachment_fetcher: base_attachment_fetcher(),
        include_loader: catapulte_outbound_mjml::include_loader::IncludeLoaderConfig::default(),
        render: catapulte::render_sandbox::RenderSandboxConfig::new(
            env!("CARGO_BIN_EXE_catapulte").into(),
        ),
        gc_sweep_interval: Duration::from_hours(1),
        gc_grace_period: Duration::from_hours(1),
        max_message_bytes: 26_214_400,
        allowed_sender_domains: None,
        bounce: None,
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
        if let Ok(resp) = client.get(&messages_url).send().await
            && let Ok(body) = resp.json::<serde_json::Value>().await
            && body["messages"].as_array().is_some_and(|a| !a.is_empty())
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // poll GET /emails/{id}/events until the delivery.succeeded event appears
    let events_url = format!("http://127.0.0.1:{http_port}/emails/{id}/events");
    let mut found_sent = false;
    for _ in 0..100 {
        if let Ok(resp) = client.get(&events_url).send().await
            && let Ok(body) = resp.json::<serde_json::Value>().await
        {
            let events = body["events"].as_array().cloned().unwrap_or_default();
            if events
                .iter()
                .any(|e| e["event_type"].as_str() == Some("delivery.succeeded"))
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
#[allow(clippy::too_many_lines)]
async fn multi_sender_falls_back_to_backup_when_primary_fails() {
    let (_mailpit, smtp_port, api_port) = start_mailpit_ready().await;

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
            auth: None,
            feedback_secret: None,
            request_timeout: std::time::Duration::from_secs(30),
        },
        inbound_nats: None,
        smtp,
        resolver: base_resolver(),
        worker: WorkerConfig::default(),
        queue: QueueBackendConfig::Storage,
        publisher: PublisherAdapterConfig::storage_only(),
        attachment_store: base_attachment_store(),
        attachment_fetcher: base_attachment_fetcher(),
        include_loader: catapulte_outbound_mjml::include_loader::IncludeLoaderConfig::default(),
        render: catapulte::render_sandbox::RenderSandboxConfig::new(
            env!("CARGO_BIN_EXE_catapulte").into(),
        ),
        gc_sweep_interval: Duration::from_hours(1),
        gc_grace_period: Duration::from_hours(1),
        max_message_bytes: 26_214_400,
        allowed_sender_domains: None,
        bounce: None,
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
        if let Ok(resp) = client.get(&messages_url).send().await
            && let Ok(body) = resp.json::<serde_json::Value>().await
            && body["messages"].as_array().is_some_and(|a| !a.is_empty())
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // poll GET /emails/{id}/events until the delivery.succeeded event appears
    let events_url = format!("http://127.0.0.1:{http_port}/emails/{id}/events");
    let mut found_sent = false;
    for _ in 0..100 {
        if let Ok(resp) = client.get(&events_url).send().await
            && let Ok(body) = resp.json::<serde_json::Value>().await
        {
            let events = body["events"].as_array().cloned().unwrap_or_default();
            if events
                .iter()
                .any(|e| e["event_type"].as_str() == Some("delivery.succeeded"))
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
async fn sent_email_blob_is_deleted_after_delivery() {
    use base64::Engine as _;

    let (_mailpit, smtp_port, api_port) = start_mailpit_ready().await;

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
            root: attachment_dir.path().to_path_buf(),
        }),
        attachment_fetcher: base_attachment_fetcher(),
        include_loader: catapulte_outbound_mjml::include_loader::IncludeLoaderConfig::default(),
        render: catapulte::render_sandbox::RenderSandboxConfig::new(
            env!("CARGO_BIN_EXE_catapulte").into(),
        ),
        gc_sweep_interval: Duration::from_hours(1),
        gc_grace_period: Duration::from_hours(1),
        max_message_bytes: 26_214_400,
        allowed_sender_domains: None,
        bounce: None,
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
        if let Ok(resp) = client.get(&messages_url).send().await
            && let Ok(body) = resp.json::<serde_json::Value>().await
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
            .filter_map(std::result::Result::ok)
            .filter(|e| {
                let name = e.file_name();
                let n = name.to_string_lossy();
                !n.starts_with('.') && e.file_type().is_ok_and(|ft| ft.is_file())
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

    let (_mailpit, smtp_port, api_port) = start_mailpit_ready().await;

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
        render: catapulte::render_sandbox::RenderSandboxConfig::new(
            env!("CARGO_BIN_EXE_catapulte").into(),
        ),
        gc_sweep_interval: Duration::from_hours(1),
        gc_grace_period: Duration::from_hours(1),
        max_message_bytes: 26_214_400,
        allowed_sender_domains: None,
        bounce: None,
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
        if let Ok(resp) = client.get(&messages_url).send().await
            && let Ok(body) = resp.json::<serde_json::Value>().await
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
#[allow(clippy::too_many_lines)]
async fn submit_via_inbound_nats_delivers_and_emits_lifecycle_event_with_correlation_id() {
    use futures_util::StreamExt as _;

    let nats = start_nats().await;
    let (_mailpit, smtp_port, api_port) = start_mailpit_ready().await;

    let nats_port = nats.get_host_port_ipv4(4222).await.unwrap();
    wait_for_tcp(nats_port, Duration::from_secs(15)).await;

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
            auth: None,
            feedback_secret: None,
            request_timeout: std::time::Duration::from_secs(30),
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
        worker: WorkerConfig::default(),
        queue: QueueBackendConfig::Storage,
        publisher: PublisherAdapterConfig::with_nats_events(
            nats_url.clone(),
            "catapulte.lifecycle".to_owned(),
        ),
        attachment_store: base_attachment_store(),
        attachment_fetcher: base_attachment_fetcher(),
        include_loader: catapulte_outbound_mjml::include_loader::IncludeLoaderConfig::default(),
        render: catapulte::render_sandbox::RenderSandboxConfig::new(
            env!("CARGO_BIN_EXE_catapulte").into(),
        ),
        gc_sweep_interval: Duration::from_hours(1),
        gc_grace_period: Duration::from_hours(1),
        max_message_bytes: 26_214_400,
        allowed_sender_domains: None,
        bounce: None,
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
        if let Ok(resp) = client.get(&messages_url).send().await
            && let Ok(body) = resp.json::<serde_json::Value>().await
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

    // Collect lifecycle events and look for a delivery.succeeded event with the expected correlation_id.
    let mut found_sent = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, lifecycle_sub.next()).await {
            Ok(Some(msg)) => {
                if let Ok(body) = serde_json::from_slice::<serde_json::Value>(&msg.payload)
                    && body["event_type"].as_str() == Some("delivery.succeeded")
                    && body["payload"]["correlation_id"].as_str() == Some("corr-e2e-test")
                {
                    found_sent = true;
                    break;
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
            auth: None,
            feedback_secret: None,
            request_timeout: std::time::Duration::from_secs(30),
        },
        inbound_nats: None,
        smtp: base_smtp(1025),
        resolver: base_resolver(),
        worker: WorkerConfig::default(),
        queue: QueueBackendConfig::Storage,
        publisher: PublisherAdapterConfig::storage_only(),
        attachment_store: AttachmentStoreBackendConfig::Fs(FsAttachmentStoreConfig {
            root: attachment_dir.path().to_path_buf(),
        }),
        attachment_fetcher: base_attachment_fetcher(),
        include_loader: catapulte_outbound_mjml::include_loader::IncludeLoaderConfig::default(),
        render: catapulte::render_sandbox::RenderSandboxConfig::new(
            env!("CARGO_BIN_EXE_catapulte").into(),
        ),
        gc_sweep_interval: Duration::from_hours(1),
        gc_grace_period: Duration::from_hours(1),
        max_message_bytes: 26_214_400,
        allowed_sender_domains: None,
        bounce: None,
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

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn multi_sender_routes_by_sender_domain() {
    let (_mailpit, smtp_port, api_port) = start_mailpit_ready().await;

    let db_dir = tempfile::tempdir().unwrap();
    let db_path = db_dir.path().join("catapulte_e2e_domain_routing.db");
    let http_port = free_port();

    // transactional: priority 1, only matches transactional.example.com
    // marketing: priority 2, catch-all (no domain restriction)
    let smtp = MultiSenderConfig::empty()
        .with_sender_domain(
            "transactional",
            SmtpConfig {
                host: "127.0.0.1".to_owned(),
                port: smtp_port,
                username: None,
                password: None,
                tls: SmtpTls::None,
            },
            1,
            None,
            Some("transactional.example.com".to_owned()),
        )
        .with_sender_domain(
            "marketing",
            SmtpConfig {
                host: "127.0.0.1".to_owned(),
                port: smtp_port,
                username: None,
                password: None,
                tls: SmtpTls::None,
            },
            2,
            None,
            None,
        );

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
        smtp,
        resolver: base_resolver(),
        worker: WorkerConfig::default(),
        queue: QueueBackendConfig::Storage,
        publisher: PublisherAdapterConfig::storage_only(),
        attachment_store: base_attachment_store(),
        attachment_fetcher: base_attachment_fetcher(),
        include_loader: catapulte_outbound_mjml::include_loader::IncludeLoaderConfig::default(),
        render: catapulte::render_sandbox::RenderSandboxConfig::new(
            env!("CARGO_BIN_EXE_catapulte").into(),
        ),
        gc_sweep_interval: Duration::from_hours(1),
        gc_grace_period: Duration::from_hours(1),
        max_message_bytes: 26_214_400,
        allowed_sender_domains: None,
        bounce: None,
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

    // Submit email from the transactional domain — should be handled by transactional sender.
    let resp1 = client
        .post(format!("http://127.0.0.1:{http_port}/emails"))
        .json(&serde_json::json!({
            "sender": "noreply@transactional.example.com",
            "recipients": [{ "kind": "to", "address": "recipient@example.com" }],
            "body": { "kind": "plain", "text": "transactional domain routing test" },
            "variables": {}
        }))
        .send()
        .await
        .expect("POST /emails failed");
    assert!(
        resp1.status().is_success(),
        "POST failed: {}",
        resp1.status()
    );
    let body1: serde_json::Value = resp1.json().await.unwrap();
    let id1 = body1["id"].as_str().expect("id missing").to_owned();

    // Submit email from a non-matched domain — should fall through to marketing (catch-all).
    let resp2 = client
        .post(format!("http://127.0.0.1:{http_port}/emails"))
        .json(&serde_json::json!({
            "sender": "bulk@other-domain.example.com",
            "recipients": [{ "kind": "to", "address": "recipient@example.com" }],
            "body": { "kind": "plain", "text": "marketing domain routing test" },
            "variables": {}
        }))
        .send()
        .await
        .expect("POST /emails failed");
    assert!(
        resp2.status().is_success(),
        "POST failed: {}",
        resp2.status()
    );
    let body2: serde_json::Value = resp2.json().await.unwrap();
    let id2 = body2["id"].as_str().expect("id missing").to_owned();

    // Wait for both emails to arrive in mailpit.
    let messages_url = format!("http://127.0.0.1:{api_port}/api/v1/messages");
    for _ in 0..100 {
        if let Ok(resp) = client.get(&messages_url).send().await
            && let Ok(body) = resp.json::<serde_json::Value>().await
            && body["messages"].as_array().is_some_and(|a| a.len() >= 2)
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // Wait for both sent events.
    for id in [&id1, &id2] {
        let events_url = format!("http://127.0.0.1:{http_port}/emails/{id}/events");
        let mut found_sent = false;
        for _ in 0..100 {
            if let Ok(resp) = client.get(&events_url).send().await
                && let Ok(body) = resp.json::<serde_json::Value>().await
            {
                let events = body["events"].as_array().cloned().unwrap_or_default();
                if events
                    .iter()
                    .any(|e| e["event_type"].as_str() == Some("delivery.succeeded"))
                {
                    found_sent = true;
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(found_sent, "no 'sent' event for email {id}");
    }

    // Check /senders: transactional should have sent_in_range==1, marketing==1.
    let senders_resp = client
        .get(format!("http://127.0.0.1:{http_port}/senders"))
        .send()
        .await
        .expect("GET /senders failed");
    assert_eq!(senders_resp.status(), 200);
    let senders_body: serde_json::Value = senders_resp.json().await.unwrap();
    let senders = senders_body["senders"].as_array().expect("senders array");

    let transactional = senders
        .iter()
        .find(|s| s["name"].as_str() == Some("transactional"))
        .expect("transactional sender not found");
    let marketing = senders
        .iter()
        .find(|s| s["name"].as_str() == Some("marketing"))
        .expect("marketing sender not found");

    assert_eq!(
        transactional["sent_in_range"].as_u64(),
        Some(1),
        "transactional should have sent_in_range == 1"
    );
    assert_eq!(
        marketing["sent_in_range"].as_u64(),
        Some(1),
        "marketing should have sent_in_range == 1"
    );
}

#[tokio::test]
async fn sink_sender_reaches_delivery_succeeded_without_any_smtp_server() {
    let db_dir = tempfile::tempdir().unwrap();
    let db_path = db_dir.path().join("catapulte_e2e_sink.db");
    let http_port = free_port();

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
        smtp: MultiSenderConfig::empty().with_sink_sender("ci", 100),
        resolver: base_resolver(),
        worker: WorkerConfig::default(),
        queue: QueueBackendConfig::Storage,
        publisher: PublisherAdapterConfig::storage_only(),
        attachment_store: base_attachment_store(),
        attachment_fetcher: base_attachment_fetcher(),
        include_loader: catapulte_outbound_mjml::include_loader::IncludeLoaderConfig::default(),
        render: catapulte::render_sandbox::RenderSandboxConfig::new(
            env!("CARGO_BIN_EXE_catapulte").into(),
        ),
        gc_sweep_interval: Duration::from_hours(1),
        gc_grace_period: Duration::from_hours(1),
        max_message_bytes: 26_214_400,
        allowed_sender_domains: None,
        bounce: None,
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
            "body": { "kind": "plain", "text": "discarded by the sink" },
            "variables": {}
        }))
        .send()
        .await
        .expect("POST /emails failed");
    assert!(resp.status().is_success(), "POST failed: {}", resp.status());
    let body: serde_json::Value = resp.json().await.unwrap();
    let id = body["id"].as_str().expect("id field missing").to_owned();

    let events_url = format!("http://127.0.0.1:{http_port}/emails/{id}/events");
    let mut delivered = false;
    for _ in 0..100 {
        if let Ok(resp) = client.get(&events_url).send().await
            && let Ok(body) = resp.json::<serde_json::Value>().await
            && body["events"].as_array().is_some_and(|events| {
                events
                    .iter()
                    .any(|e| e["event_type"].as_str() == Some("delivery.succeeded"))
            })
        {
            delivered = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(delivered, "no delivery.succeeded event with a sink sender");
}

fn named_key(name: &str, secret: &str, admin: bool, domains: Option<&[&str]>) -> NamedApiKey {
    NamedApiKey {
        name: name.to_owned(),
        secret: secret.to_owned(),
        admin,
        sender_domains: domains.map(|d| d.iter().map(|s| (*s).to_owned()).collect()),
    }
}

/// Starts the app with a sink sender and the given auth; returns the HTTP base URL.
async fn start_sink_app(auth: Option<AuthConfig>, db_dir: &tempfile::TempDir) -> String {
    start_app(auth, None, None, db_dir).await
}

async fn start_app(
    auth: Option<AuthConfig>,
    feedback_secret: Option<&str>,
    bounce: Option<catapulte_domain::entity::return_path::BounceConfig>,
    db_dir: &tempfile::TempDir,
) -> String {
    start_app_with_resolver(auth, feedback_secret, bounce, base_resolver(), db_dir).await
}

async fn start_app_with_resolver(
    auth: Option<AuthConfig>,
    feedback_secret: Option<&str>,
    bounce: Option<catapulte_domain::entity::return_path::BounceConfig>,
    resolver: TemplateResolverConfig,
    db_dir: &tempfile::TempDir,
) -> String {
    start_app_with_render(
        auth,
        feedback_secret,
        bounce,
        resolver,
        base_render(),
        db_dir,
    )
    .await
}

fn base_render() -> catapulte::render_sandbox::RenderSandboxConfig {
    catapulte::render_sandbox::RenderSandboxConfig::new(env!("CARGO_BIN_EXE_catapulte").into())
}

async fn start_app_with_render(
    auth: Option<AuthConfig>,
    feedback_secret: Option<&str>,
    bounce: Option<catapulte_domain::entity::return_path::BounceConfig>,
    resolver: TemplateResolverConfig,
    render: catapulte::render_sandbox::RenderSandboxConfig,
    db_dir: &tempfile::TempDir,
) -> String {
    let http_port = free_port();
    let config = AppConfig {
        storage: StorageBackendConfig::Sqlite(SqliteConfig {
            url: format!("sqlite:{}", db_dir.path().join("scoped.db").display()),
        }),
        http: InboundHttpConfig {
            address: format!("127.0.0.1:{http_port}").parse().unwrap(),
            auth: auth.map(std::sync::Arc::new),
            feedback_secret: feedback_secret.map(std::sync::Arc::from),
            request_timeout: std::time::Duration::from_secs(30),
        },
        inbound_nats: None,
        smtp: MultiSenderConfig::empty().with_sink_sender("ci", 100),
        resolver,
        worker: WorkerConfig::default(),
        queue: QueueBackendConfig::Storage,
        publisher: PublisherAdapterConfig::storage_only(),
        attachment_store: AttachmentStoreBackendConfig::Fs(FsAttachmentStoreConfig {
            root: db_dir.path().join("attachments"),
        }),
        attachment_fetcher: base_attachment_fetcher(),
        include_loader: catapulte_outbound_mjml::include_loader::IncludeLoaderConfig::default(),
        render,
        gc_sweep_interval: Duration::from_hours(1),
        gc_grace_period: Duration::from_hours(1),
        max_message_bytes: 26_214_400,
        allowed_sender_domains: None,
        bounce,
    };
    let app = config.build().await.expect("failed to build app");
    tokio::spawn(async move {
        let _ = app.run().await;
    });
    let base = format!("http://127.0.0.1:{http_port}");
    for _ in 0..100 {
        if reqwest::get(format!("{base}/health/live")).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    base
}

async fn submit_as(
    base: &str,
    token: &str,
    sender: &str,
    idempotency_key: Option<&str>,
    extra: serde_json::Value,
) -> reqwest::Response {
    let mut body = serde_json::json!({
        "sender": sender,
        "recipients": [{ "kind": "to", "address": "recipient@example.com" }],
        "body": { "kind": "plain", "text": "hello" },
        "variables": {}
    });
    if let Some(key) = idempotency_key {
        body["idempotency_key"] = key.into();
    }
    for (k, v) in extra.as_object().into_iter().flatten() {
        body[k] = v.clone();
    }
    reqwest::Client::new()
        .post(format!("{base}/emails"))
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .expect("POST /emails failed")
}

async fn id_of(response: reqwest::Response) -> String {
    assert!(response.status().is_success(), "{}", response.status());
    let body: serde_json::Value = response.json().await.unwrap();
    body["id"].as_str().unwrap().to_owned()
}

async fn get_json(base: &str, token: &str, path: &str) -> serde_json::Value {
    reqwest::Client::new()
        .get(format!("{base}{path}"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

async fn wait_for_delivery(base: &str, token: &str, id: &str) {
    for _ in 0..100 {
        let body = get_json(base, token, &format!("/emails/{id}/events")).await;
        if body["events"].as_array().is_some_and(|events| {
            events
                .iter()
                .any(|e| e["event_type"] == "delivery.succeeded")
        }) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("email {id} was not delivered");
}

fn ids(body: &serde_json::Value, field: &str, id_field: &str) -> Vec<String> {
    body[field]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e[id_field].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn scoped_keys_isolate_idempotency_and_reads() {
    let db_dir = tempfile::tempdir().unwrap();
    let auth = AuthConfig::new(
        Some("legacy-secret".to_owned()),
        vec![
            named_key("a", "secret-a", false, None),
            named_key("b", "secret-b", false, None),
            named_key("ops", "secret-ops", true, None),
        ],
    )
    .unwrap();
    let base = start_sink_app(auth, &db_dir).await;

    let a1 = id_of(
        submit_as(
            &base,
            "secret-a",
            "s@example.com",
            Some("k"),
            serde_json::json!({}),
        )
        .await,
    )
    .await;
    let b1 = id_of(
        submit_as(
            &base,
            "secret-b",
            "s@example.com",
            Some("k"),
            serde_json::json!({}),
        )
        .await,
    )
    .await;
    let a2 = id_of(
        submit_as(
            &base,
            "secret-a",
            "s@example.com",
            Some("k"),
            serde_json::json!({}),
        )
        .await,
    )
    .await;
    let l1 = id_of(
        submit_as(
            &base,
            "legacy-secret",
            "s@example.com",
            Some("k"),
            serde_json::json!({}),
        )
        .await,
    )
    .await;
    let l2 = id_of(
        submit_as(
            &base,
            "legacy-secret",
            "s@example.com",
            Some("k"),
            serde_json::json!({}),
        )
        .await,
    )
    .await;
    assert_ne!(
        a1, b1,
        "same idempotency key under different keys is two emails"
    );
    assert_eq!(a1, a2, "the same key resubmitting gets its own id back");
    assert_eq!(l1, l2);
    assert_ne!(l1, a1);
    for id in [&a1, &b1, &l1] {
        wait_for_delivery(&base, "secret-ops", id).await;
    }

    let a_emails = get_json(&base, "secret-a", "/emails").await;
    assert_eq!(ids(&a_emails, "emails", "id"), vec![a1.clone()]);
    let b_emails = get_json(&base, "secret-b", "/emails").await;
    assert_eq!(ids(&b_emails, "emails", "id"), vec![b1.clone()]);
    let foreign = get_json(&base, "secret-a", &format!("/emails?id={b1}")).await;
    assert!(foreign["emails"].as_array().unwrap().is_empty());

    let a_events = get_json(&base, "secret-a", "/events").await;
    assert!(
        ids(&a_events, "events", "email_id")
            .iter()
            .all(|e| *e == a1),
        "a only sees its own events"
    );
    assert!(!a_events["events"].as_array().unwrap().is_empty());
    let foreign_events = get_json(&base, "secret-a", &format!("/emails/{b1}/events")).await;
    assert!(foreign_events["events"].as_array().unwrap().is_empty());
    let own_events = get_json(&base, "secret-b", &format!("/emails/{b1}/events")).await;
    assert!(!own_events["events"].as_array().unwrap().is_empty());

    let ops_emails = get_json(&base, "secret-ops", "/emails").await;
    assert_eq!(ids(&ops_emails, "emails", "id").len(), 3);
    let legacy_emails = get_json(&base, "legacy-secret", "/emails").await;
    assert_eq!(ids(&legacy_emails, "emails", "id").len(), 3);
}

#[tokio::test]
async fn named_keys_without_a_legacy_key_still_require_a_token() {
    let db_dir = tempfile::tempdir().unwrap();
    let auth = AuthConfig::new(None, vec![named_key("a", "secret-a", false, None)]).unwrap();
    let base = start_sink_app(auth, &db_dir).await;

    let anonymous = reqwest::get(format!("{base}/emails")).await.unwrap();
    assert_eq!(anonymous.status(), 401);
    let wrong = submit_as(&base, "nope", "s@example.com", None, serde_json::json!({})).await;
    assert_eq!(wrong.status(), 401);
}

#[tokio::test]
async fn named_key_sender_domains_restrict_the_sender() {
    let db_dir = tempfile::tempdir().unwrap();
    let auth = AuthConfig::new(
        None,
        vec![named_key("a", "secret-a", false, Some(&["allowed.com"]))],
    )
    .unwrap();
    let base = start_sink_app(auth, &db_dir).await;

    let denied = submit_as(
        &base,
        "secret-a",
        "s@example.com",
        None,
        serde_json::json!({}),
    )
    .await;
    assert_eq!(denied.status(), 403);
    let allowed = submit_as(
        &base,
        "secret-a",
        "s@allowed.com",
        None,
        serde_json::json!({}),
    )
    .await;
    assert!(allowed.status().is_success());
}

#[tokio::test]
async fn identity_fields_in_the_body_cannot_change_the_caller() {
    let db_dir = tempfile::tempdir().unwrap();
    let auth = AuthConfig::new(
        None,
        vec![
            named_key("a", "secret-a", false, Some(&["allowed.com"])),
            named_key("b", "secret-b", false, None),
        ],
    )
    .unwrap();
    let base = start_sink_app(auth, &db_dir).await;
    let b1 = id_of(
        submit_as(
            &base,
            "secret-b",
            "s@example.com",
            None,
            serde_json::json!({}),
        )
        .await,
    )
    .await;

    let forged = serde_json::json!({
        "owner": "b",
        "admin": true,
        "api_key": "secret-b",
        "sender_domains": ["example.com"]
    });
    let denied = submit_as(&base, "secret-a", "s@example.com", None, forged.clone()).await;
    assert_eq!(
        denied.status(),
        403,
        "the key's own sender list still applies"
    );
    let a1 = id_of(submit_as(&base, "secret-a", "s@allowed.com", None, forged).await).await;

    let a_emails = get_json(&base, "secret-a", "/emails").await;
    assert_eq!(
        ids(&a_emails, "emails", "id"),
        vec![a1],
        "stored under a, and a stays non-admin"
    );
    assert!(!ids(&a_emails, "emails", "id").contains(&b1));
}

fn suppression_auth() -> Option<AuthConfig> {
    AuthConfig::new(
        None,
        vec![
            named_key("ops", "secret-ops", true, None),
            named_key("a", "secret-a", false, None),
        ],
    )
    .unwrap()
}

async fn send(
    method: reqwest::Method,
    base: &str,
    token: &str,
    path: &str,
    body: Option<serde_json::Value>,
) -> reqwest::Response {
    let mut request = reqwest::Client::new()
        .request(method, format!("{base}{path}"))
        .bearer_auth(token);
    if let Some(body) = body {
        request = request.json(&body);
    }
    request.send().await.unwrap()
}

#[tokio::test]
async fn suppression_routes_are_admin_only() {
    let db_dir = tempfile::tempdir().unwrap();
    let base = start_sink_app(suppression_auth(), &db_dir).await;
    let add = serde_json::json!({ "suppressions": [{ "address": "x@example.com" }] });

    for (method, path, body) in [
        (reqwest::Method::GET, "/suppressions", None),
        (reqwest::Method::POST, "/suppressions", Some(add.clone())),
        (reqwest::Method::DELETE, "/suppressions/x@example.com", None),
    ] {
        let denied = send(method, &base, "secret-a", path, body).await;
        assert_eq!(denied.status(), 403, "{path}");
    }

    let added = send(
        reqwest::Method::POST,
        &base,
        "secret-ops",
        "/suppressions",
        Some(add),
    )
    .await;
    assert_eq!(added.status(), 204);
    let bad = send(
        reqwest::Method::POST,
        &base,
        "secret-ops",
        "/suppressions",
        Some(serde_json::json!({ "suppressions": [{ "address": "not an address" }] })),
    )
    .await;
    assert_eq!(bad.status(), 400);
    let listed = get_json(&base, "secret-ops", "/suppressions?reason=manual").await;
    assert_eq!(
        ids(&listed, "suppressions", "address"),
        vec!["x@example.com"]
    );
    let deleted = send(
        reqwest::Method::DELETE,
        &base,
        "secret-ops",
        "/suppressions/X@Example.com",
        None,
    )
    .await;
    assert_eq!(deleted.status(), 204);
    let missing = send(
        reqwest::Method::DELETE,
        &base,
        "secret-ops",
        "/suppressions/x@example.com",
        None,
    )
    .await;
    assert_eq!(missing.status(), 404);
}

#[tokio::test]
async fn suppressed_recipients_are_rejected_dropped_and_released_by_delete() {
    let db_dir = tempfile::tempdir().unwrap();
    let base = start_sink_app(suppression_auth(), &db_dir).await;
    let added = send(
        reqwest::Method::POST,
        &base,
        "secret-ops",
        "/suppressions",
        Some(serde_json::json!({
            "suppressions": [{ "address": "Gone@Example.com", "reason": "bounce" }]
        })),
    )
    .await;
    assert_eq!(added.status(), 204);
    let only_gone = serde_json::json!({
        "recipients": [{ "kind": "to", "address": "gone@example.com" }]
    });

    for _ in 0..2 {
        let rejected = submit_as(
            &base,
            "secret-a",
            "s@example.com",
            Some("k"),
            only_gone.clone(),
        )
        .await;
        assert_eq!(rejected.status(), 422);
    }
    let none = get_json(&base, "secret-a", "/emails").await;
    assert!(
        none["emails"].as_array().unwrap().is_empty(),
        "no reservation is left behind"
    );

    let deleted = send(
        reqwest::Method::DELETE,
        &base,
        "secret-ops",
        "/suppressions/gone@example.com",
        None,
    )
    .await;
    assert_eq!(deleted.status(), 204);
    let id = id_of(
        submit_as(
            &base,
            "secret-a",
            "s@example.com",
            Some("k"),
            only_gone.clone(),
        )
        .await,
    )
    .await;
    wait_for_delivery(&base, "secret-ops", &id).await;

    let added = send(
        reqwest::Method::POST,
        &base,
        "secret-ops",
        "/suppressions",
        Some(serde_json::json!({ "suppressions": [{ "address": "gone@example.com" }] })),
    )
    .await;
    assert_eq!(added.status(), 204);
    let again =
        id_of(submit_as(&base, "secret-a", "s@example.com", Some("k"), only_gone).await).await;
    assert_eq!(
        again, id,
        "a later suppression never turns a resubmit into a 422"
    );

    let mixed = serde_json::json!({
        "recipients": [
            { "kind": "to", "address": "gone@example.com" },
            { "kind": "to", "address": "kept@example.com" }
        ]
    });
    let partial = id_of(submit_as(&base, "secret-a", "s@example.com", None, mixed).await).await;
    wait_for_delivery(&base, "secret-ops", &partial).await;
    let events = get_json(&base, "secret-ops", &format!("/emails/{partial}/events")).await;
    let types = ids(&events, "events", "event_type");
    assert!(types.contains(&"suppressed".to_owned()), "{types:?}");
    let status = get_json(&base, "secret-ops", &format!("/emails?id={partial}")).await;
    assert_eq!(status["emails"][0]["status"], "sent");
}

#[tokio::test]
async fn idn_forms_of_a_suppressed_address_match_at_submit_and_delete() {
    let db_dir = tempfile::tempdir().unwrap();
    let base = start_sink_app(suppression_auth(), &db_dir).await;
    let added = send(
        reqwest::Method::POST,
        &base,
        "secret-ops",
        "/suppressions",
        Some(serde_json::json!({ "suppressions": [{ "address": "u@bücher.example" }] })),
    )
    .await;
    assert_eq!(added.status(), 204);
    let listed = get_json(
        &base,
        "secret-ops",
        "/suppressions?address=U@BÜCHER.example",
    )
    .await;
    assert_eq!(
        ids(&listed, "suppressions", "address"),
        vec!["u@xn--bcher-kva.example"]
    );

    let other_form = serde_json::json!({
        "recipients": [{ "kind": "to", "address": "U@xn--bcher-kva.example" }]
    });
    let rejected = submit_as(&base, "secret-a", "s@example.com", None, other_form).await;
    assert_eq!(rejected.status(), 422);

    let deleted = send(
        reqwest::Method::DELETE,
        &base,
        "secret-ops",
        "/suppressions/u@xn--bcher-kva.example",
        None,
    )
    .await;
    assert_eq!(deleted.status(), 204);
}

async fn submit_multipart(
    base: &str,
    token: &str,
    envelope: &serde_json::Value,
) -> reqwest::Response {
    let body = format!(
        "--B\r\nContent-Disposition: form-data; name=\"envelope\"\r\nContent-Type: application/json\r\n\r\n{envelope}\r\n--B--\r\n"
    );
    reqwest::Client::new()
        .post(format!("{base}/emails"))
        .bearer_auth(token)
        .header("Content-Type", "multipart/form-data; boundary=B")
        .body(body)
        .send()
        .await
        .unwrap()
}

#[tokio::test]
async fn tags_and_metadata_are_validated_filtered_and_echoed_in_events() {
    let db_dir = tempfile::tempdir().unwrap();
    let base = start_sink_app(suppression_auth(), &db_dir).await;
    let too_many = serde_json::json!({ "tags": ["a", "b", "c", "d", "e", "f"] });
    let bad_json = submit_as(&base, "secret-a", "s@example.com", None, too_many).await;
    assert_eq!(bad_json.status(), 400);
    let bad_multipart = submit_multipart(
        &base,
        "secret-a",
        &serde_json::json!({
            "sender": "s@example.com",
            "recipients": [{ "kind": "to", "address": "r@example.com" }],
            "body": { "kind": "plain", "text": "hi" },
            "tags": ["Bad Tag"]
        }),
    )
    .await;
    assert_eq!(bad_multipart.status(), 400);

    let tagged = serde_json::json!({ "tags": ["welcome"], "metadata": { "order": "42" } });
    let id = id_of(submit_as(&base, "secret-a", "s@example.com", None, tagged).await).await;
    let other = id_of(
        submit_as(
            &base,
            "secret-a",
            "s@example.com",
            None,
            serde_json::json!({}),
        )
        .await,
    )
    .await;
    wait_for_delivery(&base, "secret-a", &id).await;
    wait_for_delivery(&base, "secret-a", &other).await;

    let listed = get_json(&base, "secret-a", "/emails?tag=welcome").await;
    assert_eq!(ids(&listed, "emails", "id"), vec![id.clone()]);
    assert_eq!(listed["emails"][0]["tags"], serde_json::json!(["welcome"]));
    assert_eq!(listed["emails"][0]["metadata"]["order"], "42");
    let hidden = get_json(&base, "secret-ops", "/emails?tag=welcome").await;
    assert_eq!(ids(&hidden, "emails", "id"), vec![id.clone()]);

    let events = get_json(&base, "secret-a", &format!("/emails/{id}/events")).await;
    let payloads: Vec<_> = events["events"].as_array().unwrap().iter().collect();
    assert!(
        payloads.len() >= 3,
        "queued, sending and delivery.succeeded"
    );
    for event in payloads {
        assert_eq!(
            event["payload"]["tags"],
            serde_json::json!(["welcome"]),
            "{event}"
        );
        assert_eq!(event["payload"]["metadata"]["order"], "42");
    }
    let plain = get_json(&base, "secret-a", &format!("/emails/{other}/events")).await;
    for event in plain["events"].as_array().unwrap() {
        assert!(event["payload"].get("tags").is_none(), "{event}");
    }
}

#[tokio::test]
async fn control_characters_in_metadata_are_rejected_per_item_and_on_single_submit() {
    let db_dir = tempfile::tempdir().unwrap();
    let base = start_sink_app(suppression_auth(), &db_dir).await;
    for metadata in [
        serde_json::json!({ "k": "a\u{0}b" }),
        serde_json::json!({ "a\u{0}b": "v" }),
        serde_json::json!({ "k": "a\u{7}b" }),
    ] {
        let single = submit_as(
            &base,
            "secret-a",
            "s@example.com",
            None,
            serde_json::json!({ "metadata": metadata }),
        )
        .await;
        assert_eq!(single.status(), 400, "{metadata}");
    }

    let email = |metadata: serde_json::Value| {
        serde_json::json!({
            "sender": "s@example.com",
            "recipients": [{ "kind": "to", "address": "r@example.com" }],
            "body": { "kind": "plain", "text": "hi" },
            "metadata": metadata
        })
    };
    let batch = send(
        reqwest::Method::POST,
        &base,
        "secret-a",
        "/emails/batch",
        Some(serde_json::json!({ "emails": [
            email(serde_json::json!({ "k": "ok" })),
            email(serde_json::json!({ "k": "a\u{0}b" })),
            email(serde_json::json!({ "k": "also ok" })),
        ]})),
    )
    .await;
    assert!(batch.status().is_success());
    let body: serde_json::Value = batch.json().await.unwrap();
    let statuses: Vec<_> = body["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["status"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(statuses, ["accepted", "rejected", "accepted"]);
}

#[tokio::test]
async fn nul_in_request_fields_is_rejected_per_item_and_on_single_submit() {
    let db_dir = tempfile::tempdir().unwrap();
    let base = start_sink_app(suppression_auth(), &db_dir).await;
    for extra in [
        serde_json::json!({ "subject": "a\u{0}b" }),
        serde_json::json!({ "correlation_id": "a\u{0}b" }),
        serde_json::json!({ "variables": { "k": ["a\u{0}b"] } }),
        serde_json::json!({ "body": { "kind": "plain", "text": "a\u{0}b" } }),
    ] {
        let single = submit_as(&base, "secret-a", "s@example.com", None, extra.clone()).await;
        assert_eq!(single.status(), 400, "{extra}");
    }
    let keyed = submit_as(
        &base,
        "secret-a",
        "s@example.com",
        Some("k\u{0}"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(keyed.status(), 400);

    let email = |variables: serde_json::Value| {
        serde_json::json!({
            "sender": "s@example.com",
            "recipients": [{ "kind": "to", "address": "r@example.com" }],
            "body": { "kind": "plain", "text": "hi" },
            "variables": variables
        })
    };
    let batch = send(
        reqwest::Method::POST,
        &base,
        "secret-a",
        "/emails/batch",
        Some(serde_json::json!({ "emails": [
            email(serde_json::json!({ "k": "ok" })),
            email(serde_json::json!({ "k": "a\u{0}b" })),
            email(serde_json::json!({ "k": "also ok" })),
        ]})),
    )
    .await;
    assert!(batch.status().is_success());
    let body: serde_json::Value = batch.json().await.unwrap();
    let statuses: Vec<_> = body["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["status"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(statuses, ["accepted", "rejected", "accepted"]);
}

const INGEST_SECRET: &str = "ingest-secret-0123456789abcdef0123456789";
const BOUNCE_SECRET: &str = "bounce-secret-0123456789abcdef0123456789";
const DSN_FIXTURE: &str = include_str!("../../adapter/inbound-http/tests/fixtures/dsn.eml");
const ARF_FIXTURE: &str = include_str!("../../adapter/inbound-http/tests/fixtures/arf.eml");

fn bounce_config() -> catapulte_domain::entity::return_path::BounceConfig {
    catapulte_domain::entity::return_path::BounceConfig::new("bounces.example.com", BOUNCE_SECRET)
        .unwrap()
}

fn id_of_uuid(id: &str) -> catapulte_domain::entity::email::EmailId {
    catapulte_domain::entity::email::EmailId::from(uuid::Uuid::parse_str(id).unwrap())
}

fn fixture(template: &str, values: &[(&str, &str)]) -> String {
    let mut out = template.to_owned();
    for key in [
        "BOUNCE_TO",
        "RECIPIENT",
        "ACTION",
        "STATUS",
        "RETURN_PATH",
        "MESSAGE_ID",
        "ORIGINAL_RETURN_PATH",
    ] {
        let value = values
            .iter()
            .find(|(k, _)| *k == key)
            .map_or("", |(_, v)| *v);
        out = out.replace(&format!("{{{{{key}}}}}"), value);
    }
    out.replace('\n', "\r\n")
}

async fn post_feedback(base: &str, token: &str, body: String) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{base}/inbound/feedback"))
        .bearer_auth(token)
        .body(body)
        .send()
        .await
        .unwrap()
}

/// A DSN for `email_id` naming `recipient`, signed with `verp` as the return path.
fn dsn(email_id: &str, recipient: &str, action: &str, status: &str, verp: &str) -> String {
    fixture(
        DSN_FIXTURE,
        &[
            ("BOUNCE_TO", verp),
            ("RECIPIENT", recipient),
            ("ACTION", action),
            ("STATUS", status),
            ("RETURN_PATH", verp),
            ("MESSAGE_ID", &format!("{email_id}@example.com")),
        ],
    )
}

async fn event_types(base: &str, email_id: &str) -> Vec<String> {
    let events = get_json(base, "secret-ops", &format!("/emails/{email_id}/events")).await;
    ids(&events, "events", "event_type")
}

async fn suppressed(base: &str) -> Vec<String> {
    let listed = get_json(base, "secret-ops", "/suppressions").await;
    ids(&listed, "suppressions", "address")
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn feedback_reports_suppress_only_verified_single_recipient_emails() {
    let db_dir = tempfile::tempdir().unwrap();
    let base = start_app(
        suppression_auth(),
        Some(INGEST_SECRET),
        Some(bounce_config()),
        &db_dir,
    )
    .await;
    let bounce = bounce_config();
    let single = id_of(
        submit_as(
            &base,
            "secret-a",
            "s@example.com",
            None,
            serde_json::json!({
                "recipients": [{ "kind": "to", "address": "alice@example.org" }],
                "tags": ["welcome"]
            }),
        )
        .await,
    )
    .await;
    let pair = id_of(
        submit_as(
            &base,
            "secret-a",
            "s@example.com",
            None,
            serde_json::json!({
                "recipients": [
                    { "kind": "to", "address": "bob@example.org" },
                    { "kind": "bcc", "address": "carol@example.org" }
                ]
            }),
        )
        .await,
    )
    .await;
    wait_for_delivery(&base, "secret-ops", &single).await;
    wait_for_delivery(&base, "secret-ops", &pair).await;
    let verp_single = bounce.return_path(id_of_uuid(&single));
    let verp_pair = bounce.return_path(id_of_uuid(&pair));

    // A multi-recipient email only gets an event, even with a valid tag.
    let r = post_feedback(
        &base,
        INGEST_SECRET,
        dsn(&pair, "bob@example.org", "failed", "5.1.1", &verp_pair),
    )
    .await;
    assert_eq!(r.status(), 204);
    assert!(
        event_types(&base, &pair)
            .await
            .contains(&"bounced".to_owned())
    );
    assert!(suppressed(&base).await.is_empty());

    // Rejections leave no event and no suppression.
    let at = verp_single.find('@').unwrap();
    let last = if verp_single.as_bytes()[at - 1] == b'x' {
        'y'
    } else {
        'x'
    };
    let tampered = format!("{}{last}", &verp_single[..at - 1]) + "@bounces.example.com";
    for (body, why) in [
        (
            dsn(&single, "alice@example.org", "failed", "5.1.1", &tampered),
            "tampered tag",
        ),
        (
            dsn(
                &single,
                "alice@example.org",
                "failed",
                "5.1.1",
                "postmaster@example.com",
            ),
            "no VERP",
        ),
        (
            dsn(
                &single,
                "victim@example.org",
                "failed",
                "5.1.1",
                &verp_single,
            ),
            "recipient not on the email",
        ),
        (
            dsn(
                &uuid::Uuid::now_v7().to_string(),
                "alice@example.org",
                "failed",
                "5.1.1",
                &bounce.return_path(id_of_uuid(&uuid::Uuid::now_v7().to_string())),
            ),
            "unknown email",
        ),
        (
            fixture(
                include_str!("../../adapter/inbound-http/tests/fixtures/plain.eml"),
                &[],
            ),
            "not a report",
        ),
    ] {
        let r = post_feedback(&base, INGEST_SECRET, body).await;
        assert_eq!(r.status(), 422, "{why}");
    }
    assert!(
        !event_types(&base, &single)
            .await
            .contains(&"bounced".to_owned())
    );
    assert!(suppressed(&base).await.is_empty());

    // A soft bounce is an event only.
    let r = post_feedback(
        &base,
        INGEST_SECRET,
        dsn(
            &single,
            "alice@example.org",
            "delayed",
            "4.4.1",
            &verp_single,
        ),
    )
    .await;
    assert_eq!(r.status(), 204);
    assert!(suppressed(&base).await.is_empty());

    // A Message-ID-only complaint is an unverified event.
    let arf = fixture(
        ARF_FIXTURE,
        &[
            ("BOUNCE_TO", "feedback@example.net"),
            ("RECIPIENT", "alice@example.org"),
            ("MESSAGE_ID", &format!("{single}@example.com")),
        ],
    );
    let r = post_feedback(&base, INGEST_SECRET, arf).await;
    assert_eq!(r.status(), 204);
    let events = get_json(
        &base,
        "secret-ops",
        &format!("/emails/{single}/events?event_type=complained"),
    )
    .await;
    assert_eq!(events["events"][0]["payload"]["verified"], false);
    assert_eq!(
        events["events"][0]["payload"]["tags"],
        serde_json::json!(["welcome"])
    );
    assert!(suppressed(&base).await.is_empty());

    // A signed hard bounce for the single-recipient email suppresses.
    let r = post_feedback(
        &base,
        INGEST_SECRET,
        dsn(
            &single,
            "Alice@Example.org",
            "failed",
            "5.1.1",
            &verp_single,
        ),
    )
    .await;
    assert_eq!(r.status(), 204);
    assert_eq!(suppressed(&base).await, vec!["alice@example.org"]);
    let listed = get_json(&base, "secret-ops", "/suppressions?reason=bounce").await;
    assert_eq!(listed["suppressions"][0]["source_email_id"], single);
    let status = get_json(&base, "secret-a", &format!("/emails?id={single}")).await;
    assert_eq!(
        status["emails"][0]["status"], "sent",
        "bounced never changes the status"
    );
}

#[tokio::test]
async fn the_feedback_route_is_disabled_without_a_secret_and_rejects_api_keys_and_large_bodies() {
    let db_dir = tempfile::tempdir().unwrap();
    let off = start_app(suppression_auth(), None, Some(bounce_config()), &db_dir).await;
    let r = post_feedback(&off, INGEST_SECRET, "x".to_owned()).await;
    assert_eq!(r.status(), 404);

    let db_dir = tempfile::tempdir().unwrap();
    let base = start_app(
        suppression_auth(),
        Some(INGEST_SECRET),
        Some(bounce_config()),
        &db_dir,
    )
    .await;
    for token in ["secret-ops", "secret-a", "wrong"] {
        let r = post_feedback(&base, token, "x".to_owned()).await;
        assert_eq!(r.status(), 401, "{token}");
    }
    let anonymous = reqwest::Client::new()
        .post(format!("{base}/inbound/feedback"))
        .body("x")
        .send()
        .await
        .unwrap();
    assert_eq!(anonymous.status(), 401);
    let huge = post_feedback(&base, INGEST_SECRET, "x".repeat(1024 * 1024 + 1)).await;
    assert_eq!(huge.status(), 413);
    let not_a_report = post_feedback(&base, INGEST_SECRET, "x".to_owned()).await;
    assert_eq!(not_a_report.status(), 422);
}

const WELCOME: &str = "<mjml><mj-body><mj-section><mj-column><mj-text>Hi {{ customer.name }}</mj-text></mj-column></mj-section></mj-body></mjml>";

async fn put_template(
    base: &str,
    token: &str,
    name: &str,
    source: &str,
    validation_variables: serde_json::Value,
) -> reqwest::Response {
    send(
        reqwest::Method::PUT,
        base,
        token,
        &format!("/templates/{name}"),
        Some(serde_json::json!({ "source": source, "validation_variables": validation_variables })),
    )
    .await
}

#[tokio::test]
async fn template_routes_are_admin_only() {
    let db_dir = tempfile::tempdir().unwrap();
    let base = start_sink_app(suppression_auth(), &db_dir).await;
    let body = serde_json::json!({ "source": WELCOME });
    for (method, path, payload) in [
        (reqwest::Method::GET, "/templates", None),
        (reqwest::Method::GET, "/templates/welcome", None),
        (reqwest::Method::PUT, "/templates/welcome", Some(body)),
        (reqwest::Method::DELETE, "/templates/welcome", None),
        (
            reqwest::Method::POST,
            "/templates/welcome/preview",
            Some(serde_json::json!({})),
        ),
    ] {
        let denied = send(method, &base, "secret-a", path, payload).await;
        assert_eq!(denied.status(), 403, "{path}");
    }
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn templates_are_validated_previewed_and_used_by_sends() {
    let files = tempfile::tempdir().unwrap();
    std::fs::write(files.path().join("filed.mjml"), WELCOME).unwrap();
    let resolver = TemplateResolverConfig {
        templates_dir: Some(files.path().to_path_buf()),
        ..base_resolver()
    };
    let db_dir = tempfile::tempdir().unwrap();
    let base = start_app_with_resolver(suppression_auth(), None, None, resolver, &db_dir).await;

    // A missing nested attribute passes validation, but arithmetic needs data.
    let ok = put_template(
        &base,
        "secret-ops",
        "welcome",
        WELCOME,
        serde_json::json!({}),
    )
    .await;
    assert_eq!(ok.status(), 200);
    let doubled = WELCOME.replace("Hi {{ customer.name }}", "{{ total + 1 }}");
    let rejected = put_template(&base, "secret-ops", "math", &doubled, serde_json::json!({})).await;
    assert_eq!(rejected.status(), 400);
    let message: serde_json::Value = rejected.json().await.unwrap();
    assert!(
        message["error"].as_str().unwrap().contains("+ operator"),
        "{message}"
    );
    let with_data = put_template(
        &base,
        "secret-ops",
        "math",
        &doubled,
        serde_json::json!({ "total": 1 }),
    )
    .await;
    assert_eq!(with_data.status(), 200);
    let preview = send(
        reqwest::Method::POST,
        &base,
        "secret-ops",
        "/templates/math/preview",
        Some(serde_json::json!({ "variables": { "total": 1 } })),
    )
    .await;
    let preview: serde_json::Value = preview.json().await.unwrap();
    assert!(
        preview["html"].as_str().unwrap().contains(">2<"),
        "{preview}"
    );

    // Broken MJML is a 400 and nothing is stored.
    let broken = put_template(
        &base,
        "secret-ops",
        "broken",
        "<mjml><mj-body>",
        serde_json::json!({}),
    )
    .await;
    assert_eq!(broken.status(), 400);
    let missing = get_json(&base, "secret-ops", "/templates/broken").await;
    assert_eq!(missing["error"], "not found");

    // The filesystem is authoritative.
    let on_disk = put_template(&base, "secret-ops", "filed", WELCOME, serde_json::json!({})).await;
    assert_eq!(on_disk.status(), 409);
    let delete = send(
        reqwest::Method::DELETE,
        &base,
        "secret-ops",
        "/templates/filed",
        None,
    )
    .await;
    assert_eq!(delete.status(), 409);

    // Previews escape variables like a send.
    let escaped = send(
        reqwest::Method::POST,
        &base,
        "secret-ops",
        "/templates/welcome/preview",
        Some(serde_json::json!({ "variables": { "customer": { "name": "<script>x</script>" } } })),
    )
    .await;
    let escaped: serde_json::Value = escaped.json().await.unwrap();
    let html = escaped["html"].as_str().unwrap();
    assert!(
        !html.contains("<script>") && html.contains("&lt;script&gt;"),
        "{html}"
    );

    // New versions replace the old one, and the list shows the latest.
    let again = put_template(
        &base,
        "secret-ops",
        "welcome",
        WELCOME,
        serde_json::json!({}),
    )
    .await;
    let again: serde_json::Value = again.json().await.unwrap();
    assert_eq!(again["version"], 2);
    let listed = get_json(&base, "secret-ops", "/templates").await;
    assert_eq!(ids(&listed, "templates", "name"), vec!["math", "welcome"]);

    // A send using the stored template is delivered; a missing nested attribute still fails
    // at send time.
    let named = |variables: serde_json::Value| {
        serde_json::json!({
            "body": { "kind": "mjml_named", "name": "welcome" },
            "variables": variables
        })
    };
    let delivered = id_of(
        submit_as(
            &base,
            "secret-a",
            "s@example.com",
            None,
            named(serde_json::json!({ "customer": { "name": "Ann" } })),
        )
        .await,
    )
    .await;
    wait_for_delivery(&base, "secret-a", &delivered).await;
    let strict = id_of(
        submit_as(
            &base,
            "secret-a",
            "s@example.com",
            None,
            named(serde_json::json!({})),
        )
        .await,
    )
    .await;
    let mut failed = false;
    for _ in 0..100 {
        let events = get_json(&base, "secret-a", &format!("/emails/{strict}/events")).await;
        if events["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["error_class"] == "template_interpolate")
        {
            failed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(failed, "send-time rendering is not lenient");

    let removed = send(
        reqwest::Method::DELETE,
        &base,
        "secret-ops",
        "/templates/math",
        None,
    )
    .await;
    assert_eq!(removed.status(), 204);
}

#[tokio::test]
async fn runaway_templates_are_rejected_on_upload_and_fail_a_send_without_a_retry() {
    let hostile = "<mjml><mj-body><mj-section><mj-column><mj-text>{% for a in range(100000) %}{% for b in range(100000) %}x{% endfor %}{% endfor %}</mj-text></mj-column></mj-section></mj-body></mjml>";
    let db_dir = tempfile::tempdir().unwrap();
    let base = start_sink_app(suppression_auth(), &db_dir).await;

    let upload = put_template(&base, "secret-ops", "loop", hostile, serde_json::json!({})).await;
    assert_eq!(upload.status(), 400);
    let message: serde_json::Value = upload.json().await.unwrap();
    assert!(
        message["error"].as_str().unwrap().contains("budget"),
        "{message}"
    );

    let id = id_of(
        submit_as(
            &base,
            "secret-a",
            "s@example.com",
            None,
            serde_json::json!({ "body": { "kind": "mjml_inline", "source": hostile } }),
        )
        .await,
    )
    .await;
    let mut types = Vec::new();
    for _ in 0..100 {
        types = event_types(&base, &id).await;
        if types.contains(&"delivery.failed".to_owned()) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(types.contains(&"delivery.failed".to_owned()), "{types:?}");
    assert!(!types.contains(&"retrying".to_owned()), "{types:?}");
}

const STATIC_MJML: &str = "<mjml><mj-body><mj-section><mj-column><mj-text>Hi</mj-text></mj-column></mj-section></mj-body></mjml>";

/// `==` on two separately built towers, which the engine cannot meter.
fn hostile_body() -> String {
    let mut source = String::from("{% set a = ['x'] %}{% set b = ['x'] %}");
    source.push_str(&"{% set a = [a, a] %}{% set b = [b, b] %}".repeat(28));
    source.push_str("{{ a == b }}");
    format!(
        "<mjml><mj-body><mj-section><mj-column><mj-text>{source}</mj-text></mj-column></mj-section></mj-body></mjml>"
    )
}

async fn submit_mjml(base: &str, source: &str) -> String {
    id_of(
        submit_as(
            base,
            "secret-a",
            "s@example.com",
            None,
            serde_json::json!({ "body": { "kind": "mjml_inline", "source": source } }),
        )
        .await,
    )
    .await
}

async fn events_of(base: &str, id: &str) -> Vec<serde_json::Value> {
    get_json(base, "secret-ops", &format!("/emails/{id}/events")).await["events"]
        .as_array()
        .unwrap()
        .clone()
}

async fn wait_for_event(base: &str, id: &str, event_type: &str) -> Vec<serde_json::Value> {
    for _ in 0..200 {
        let events = events_of(base, id).await;
        if events.iter().any(|e| e["event_type"] == event_type) {
            return events;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!(
        "no {event_type} event for {id}: {:?}",
        events_of(base, id).await
    );
}

async fn preview(base: &str, name: &str) -> reqwest::Response {
    send(
        reqwest::Method::POST,
        base,
        "secret-ops",
        &format!("/templates/{name}/preview"),
        Some(serde_json::json!({ "variables": {} })),
    )
    .await
}

fn templates_dir_with(name: &str, source: &str) -> (tempfile::TempDir, TemplateResolverConfig) {
    let files = tempfile::tempdir().unwrap();
    std::fs::write(files.path().join(format!("{name}.mjml")), source).unwrap();
    let resolver = TemplateResolverConfig {
        templates_dir: Some(files.path().to_path_buf()),
        ..base_resolver()
    };
    (files, resolver)
}

#[tokio::test]
async fn a_send_with_an_unmetered_template_fails_once_as_template_interpolate_without_any_retry() {
    let db_dir = tempfile::tempdir().unwrap();
    let base = start_sink_app(suppression_auth(), &db_dir).await;

    let id = submit_mjml(&base, &hostile_body()).await;

    let events = wait_for_event(&base, &id, "delivery.failed").await;
    let failed: Vec<_> = events
        .iter()
        .filter(|e| e["event_type"] == "delivery.failed")
        .collect();
    assert_eq!(failed.len(), 1, "{events:?}");
    assert_eq!(failed[0]["payload"]["attempt"], 1, "{events:?}");
    assert_eq!(
        failed[0]["error_class"], "template_interpolate",
        "{events:?}"
    );
    assert!(
        events
            .iter()
            .all(|e| !["retrying", "delivery.succeeded"]
                .contains(&e["event_type"].as_str().unwrap())),
        "{events:?}"
    );
}

#[tokio::test]
async fn hostile_uploads_and_previews_are_rejected_with_400_long_before_the_http_timeout() {
    let (_files, resolver) = templates_dir_with("hostile", &hostile_body());
    let db_dir = tempfile::tempdir().unwrap();
    let base = start_app_with_resolver(suppression_auth(), None, None, resolver, &db_dir).await;

    let started = std::time::Instant::now();
    let upload = put_template(
        &base,
        "secret-ops",
        "stored",
        &hostile_body(),
        serde_json::json!({}),
    )
    .await;
    let previewed = preview(&base, "hostile").await;

    assert_eq!(upload.status(), 400);
    assert_eq!(previewed.status(), 400);
    let message: serde_json::Value = previewed.json().await.unwrap();
    assert!(
        message["error"].as_str().unwrap().contains("budget"),
        "{message}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "{:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn a_broken_render_program_is_a_503_for_templates_and_a_retry_for_sends() {
    let (_files, resolver) = templates_dir_with("filed", WELCOME);
    let program_dir = tempfile::tempdir_in(
        std::path::Path::new(env!("CARGO_BIN_EXE_catapulte"))
            .parent()
            .unwrap(),
    )
    .unwrap();
    let program = program_dir.path().join("catapulte");
    std::fs::hard_link(env!("CARGO_BIN_EXE_catapulte"), &program).unwrap();
    let render = catapulte::render_sandbox::RenderSandboxConfig::new(program.clone());
    let db_dir = tempfile::tempdir().unwrap();
    let base =
        start_app_with_render(suppression_auth(), None, None, resolver, render, &db_dir).await;
    std::fs::remove_file(&program).unwrap();

    let upload = put_template(&base, "secret-ops", "new", WELCOME, serde_json::json!({})).await;
    let previewed = preview(&base, "filed").await;
    let id = submit_mjml(&base, WELCOME).await;

    assert_eq!(upload.status(), 503);
    assert_eq!(previewed.status(), 503);
    let events = wait_for_event(&base, &id, "retrying").await;
    assert!(
        events.iter().all(|e| e["event_type"] != "delivery.failed"),
        "{events:?}"
    );
    let retrying = events
        .iter()
        .find(|e| e["event_type"] == "retrying")
        .unwrap();
    assert_eq!(
        retrying["error_class"], "template_interpolate",
        "{events:?}"
    );
}

#[tokio::test]
async fn previews_and_sends_share_one_admission_limit() {
    let (_files, resolver) = templates_dir_with("hostile", &hostile_body());
    let render = catapulte::render_sandbox::RenderSandboxConfig {
        concurrency: 1,
        ..base_render()
    };
    let db_dir = tempfile::tempdir().unwrap();
    let base =
        start_app_with_render(suppression_auth(), None, None, resolver, render, &db_dir).await;

    let hostile_base = base.clone();
    let hostile = tokio::spawn(async move { preview(&hostile_base, "hostile").await.status() });
    tokio::time::sleep(Duration::from_millis(400)).await;
    let id = submit_mjml(&base, STATIC_MJML).await;
    tokio::time::sleep(Duration::from_millis(900)).await;

    let during = event_types(&base, &id).await;
    assert!(
        !during.contains(&"delivery.succeeded".to_owned()),
        "{during:?}"
    );
    assert_eq!(hostile.await.unwrap(), 400);
    wait_for_event(&base, &id, "delivery.succeeded").await;
}
