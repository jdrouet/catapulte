use std::collections::HashSet;
use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;
use std::time::Duration;

use catapulte::AppConfig;
use catapulte_domain::port::email_queue::EmailQueue;
use catapulte_domain::port::event_publisher::EventPublisher;
use catapulte_domain::use_case::process_queued_email::{
    ProcessQueuedEmailService, ProcessQueuedEmailUseCase,
};
use catapulte_domain::use_case::submit_email::{SubmitEmailService, SubmitEmailUseCase};
use catapulte_inbound_http::{HttpServerState, InboundHttpConfig};
use catapulte_inbound_worker::worker::{Worker, WorkerConfig, WorkerState};
use catapulte_outbound_interpolator::interpolator::MiniJinjaInterpolator;
use catapulte_outbound_mjml::renderer::MjmlRenderer;
use catapulte_outbound_queue_memory::MemoryQueue;
use catapulte_outbound_resolver::resolver::{TemplateResolverAdapter, TemplateResolverConfig};
use catapulte_outbound_smtp::sender::{SmtpConfig, SmtpSender, SmtpTls};
use catapulte_outbound_sqlite::{SqliteAdapter, SqliteConfig};
use testcontainers::GenericImage;
use testcontainers::core::{IntoContainerPort, WaitFor};
use testcontainers::runners::AsyncRunner;

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[tokio::test]
async fn submit_plain_email_is_delivered_via_mailpit() {
    let mailpit = GenericImage::new("axllent/mailpit", "latest")
        .with_exposed_port(1025.tcp())
        .with_exposed_port(8025.tcp())
        .with_wait_for(WaitFor::message_on_stdout("[http] accessible via"))
        .start()
        .await
        .expect("failed to start mailpit container; ensure Docker is running");

    let smtp_port = mailpit.get_host_port_ipv4(1025).await.unwrap();
    let api_port = mailpit.get_host_port_ipv4(8025).await.unwrap();

    let db_dir = tempfile::tempdir().unwrap();
    let db_path = db_dir.path().join("catapulte_e2e.db");

    let http_port = free_port();
    let catapulte_addr: SocketAddr = format!("127.0.0.1:{http_port}").parse().unwrap();

    let config = AppConfig {
        sqlite: SqliteConfig {
            url: format!("sqlite:{}", db_path.display()),
        },
        http: InboundHttpConfig {
            address: catapulte_addr,
        },
        smtp: SmtpConfig {
            host: "127.0.0.1".to_owned(),
            port: smtp_port,
            username: None,
            password: None,
            tls: SmtpTls::None,
        },
        resolver: TemplateResolverConfig {
            allowed_domains: HashSet::new(),
            templates_dir: None,
        },
        worker: WorkerConfig {},
    };

    let app = config.build().await.expect("failed to build app");
    tokio::spawn(async move {
        let _ = app.run().await;
    });

    let client = reqwest::Client::new();

    // Wait for catapulte HTTP to be ready (any response means server is up)
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

    // Submit the email
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

    // Poll mailpit until the email appears (worker processes asynchronously)
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
        {
            if body["messages"].as_array().is_some_and(|a| !a.is_empty()) {
                delivered = true;
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    assert!(
        delivered,
        "email was not delivered to mailpit within timeout"
    );
}

type MemoryProcessService = ProcessQueuedEmailService<
    TemplateResolverAdapter,
    MiniJinjaInterpolator,
    MjmlRenderer,
    SmtpSender,
>;

#[derive(Clone)]
struct MemoryQueueAppState {
    submit: Arc<SubmitEmailService<SqliteAdapter, MemoryQueue>>,
    process: Arc<MemoryProcessService>,
    queue: MemoryQueue,
    sqlite: SqliteAdapter,
}

impl HttpServerState for MemoryQueueAppState {
    fn submit_email(&self) -> &impl SubmitEmailUseCase {
        self.submit.as_ref()
    }
}

impl WorkerState for MemoryQueueAppState {
    fn process_queued_email(&self) -> &impl ProcessQueuedEmailUseCase {
        self.process.as_ref()
    }

    fn email_queue(&self) -> &impl EmailQueue {
        &self.queue
    }

    fn event_publisher(&self) -> &impl EventPublisher {
        &self.sqlite
    }
}

#[tokio::test]
async fn submit_plain_email_with_memory_queue_is_delivered() {
    let mailpit = GenericImage::new("axllent/mailpit", "latest")
        .with_exposed_port(1025.tcp())
        .with_exposed_port(8025.tcp())
        .with_wait_for(WaitFor::message_on_stdout("[http] accessible via"))
        .start()
        .await
        .expect("failed to start mailpit container; ensure Docker is running");

    let smtp_port = mailpit.get_host_port_ipv4(1025).await.unwrap();
    let api_port = mailpit.get_host_port_ipv4(8025).await.unwrap();

    let sqlite = SqliteAdapter::connect(":memory:").await.unwrap();
    sqlite.migrate().await.unwrap();

    let queue = MemoryQueue::new();

    let smtp = SmtpConfig {
        host: "127.0.0.1".to_owned(),
        port: smtp_port,
        username: None,
        password: None,
        tls: SmtpTls::None,
    }
    .build()
    .unwrap();

    let resolver = TemplateResolverConfig {
        allowed_domains: HashSet::new(),
        templates_dir: None,
    }
    .build()
    .unwrap();

    let state = MemoryQueueAppState {
        submit: Arc::new(SubmitEmailService::new(sqlite.clone(), queue.clone())),
        process: Arc::new(ProcessQueuedEmailService::new(
            resolver,
            MiniJinjaInterpolator::new(),
            MjmlRenderer::new(),
            smtp,
        )),
        queue,
        sqlite,
    };

    let http_port = free_port();
    let addr: SocketAddr = format!("127.0.0.1:{http_port}").parse().unwrap();

    let server = InboundHttpConfig { address: addr }.build();
    let worker = Worker {};

    let state_for_server = state.clone();
    tokio::spawn(async move {
        let _ = server.run(state_for_server).await;
    });
    tokio::spawn(async move {
        worker.run(state).await;
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
            "body": { "kind": "plain", "text": "Hello from memory queue e2e!" },
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
        {
            if body["messages"].as_array().is_some_and(|a| !a.is_empty()) {
                delivered = true;
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    assert!(
        delivered,
        "email was not delivered to mailpit within timeout"
    );
}
