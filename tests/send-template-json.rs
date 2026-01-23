use std::path::PathBuf;

use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use catapulte_adapter_http::HttpServer;
use catapulte_adapter_smtp::{SmtpConfig, SmtpSender};
use catapulte_adapter_template::{
    LocalLoader, LocalLoaderConfig, MrmlRenderer, MrmlRendererConfig, MultiLoader,
};
use catapulte_domain::service::SendEmailService;
use testcontainers::core::{ContainerPort, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::{GenericImage, ImageExt};
use tower::ServiceExt;

fn create_service(smtp_port: u16) -> SendEmailService<MultiLoader, MrmlRenderer, SmtpSender> {
    let loader_config = LocalLoaderConfig {
        path: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("template"),
    };
    let loader = MultiLoader::new().with_local(LocalLoader::new(&loader_config));

    let renderer = MrmlRenderer::new(&MrmlRendererConfig::default());

    let smtp_config = SmtpConfig {
        hostname: "localhost".into(),
        port: smtp_port,
        ..Default::default()
    };
    let sender = SmtpSender::new(&smtp_config).expect("failed to build SMTP sender");

    SendEmailService::new(loader, renderer, sender)
}

#[tokio::test]
async fn should_submit_simple() {
    let _ = catapulte::init_logs("debug", false);

    let smtp_node = GenericImage::new("rnwood/smtp4dev", "latest")
        .with_wait_for(WaitFor::message_on_stdout(
            "Now listening on: http://[::]:80",
        ))
        .with_exposed_port(ContainerPort::Tcp(25))
        .with_exposed_port(ContainerPort::Tcp(80))
        .with_env_var("ServerOptions__BasePath", "/")
        .with_env_var("ServerOptions__TlsMode", "None")
        .start()
        .await
        .unwrap();

    let smtp_port = smtp_node.get_host_port_ipv4(25).await.unwrap();

    let service = create_service(smtp_port);
    let server = HttpServer::new(Default::default(), service);
    let app = server.router();

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/templates/user-login/json")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({
                        "to": "to-user@example.com",
                        "from": "from-user@example.com",
                        "params": {
                            "name": "Joe",
                            "token": "foo",
                        },
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
}
