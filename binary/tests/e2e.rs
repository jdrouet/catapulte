use std::collections::HashSet;
use std::net::{SocketAddr, TcpListener};
use std::time::Duration;

use catapulte::AppConfig;
use catapulte_inbound_http::InboundHttpConfig;
use catapulte_inbound_worker::worker::WorkerConfig;
use catapulte_outbound_resolver::resolver::TemplateResolverConfig;
use catapulte_outbound_smtp::sender::{SmtpConfig, SmtpTls};
use catapulte_outbound_sqlite::SqliteConfig;

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

struct MailpitGuard {
    smtp_port: u16,
    api_port: u16,
    child: std::process::Child,
}

impl MailpitGuard {
    fn start() -> Self {
        let smtp_port = free_port();
        let api_port = free_port();
        let child = std::process::Command::new("mailpit")
            .args([
                "--smtp",
                &format!("127.0.0.1:{smtp_port}"),
                "--listen",
                &format!("127.0.0.1:{api_port}"),
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("mailpit not found in PATH; install with: cargo binstall mailpit");
        Self {
            smtp_port,
            api_port,
            child,
        }
    }

    fn api_base(&self) -> String {
        format!("http://127.0.0.1:{}", self.api_port)
    }
}

impl Drop for MailpitGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

#[tokio::test]
async fn submit_plain_email_is_delivered_via_mailpit() {
    let mailpit = MailpitGuard::start();

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
            port: mailpit.smtp_port,
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

    // Wait for catapulte to be ready (any HTTP response means it's up)
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

    // Wait for mailpit API to be ready
    let messages_url = format!("{}/api/v1/messages", mailpit.api_base());
    for _ in 0..100 {
        if client.get(&messages_url).send().await.is_ok() {
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

    // Poll mailpit until the email appears
    let mut delivered = false;
    for _ in 0..100 {
        let body: serde_json::Value = client
            .get(&messages_url)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if body["messages"].as_array().is_some_and(|a| !a.is_empty()) {
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
