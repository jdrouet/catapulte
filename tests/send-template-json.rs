use std::{net::Ipv4Addr, path::PathBuf};

use axum::{
    body::Body,
    http::{header, Request, StatusCode},
};
use catapulte::service::{server, smtp};
use testcontainers::clients::Cli as DockerCli;
use testcontainers::core::WaitFor;
use testcontainers::GenericImage;
use tower::ServiceExt;

fn engine_config() -> catapulte_engine::Config {
    catapulte_engine::Config {
        loader: catapulte_engine::loader::Config {
            local: catapulte_engine::loader::local::Config {
                path: PathBuf::from("template"),
            },
            http: None,
        },
        parser: Default::default(),
        render: Default::default(),
    }
}

fn smtp_config(port: u16) -> smtp::Configuration {
    smtp::Configuration {
        hostname: "localhost".into(),
        port,
        ..Default::default()
    }
}

fn server_config(
    engine: catapulte_engine::Config,
    smtp: smtp::Configuration,
) -> server::Configuration {
    server::Configuration {
        host: Ipv4Addr::new(127, 0, 0, 1).into(),
        port: 3000,
        engine,
        smtp,
    }
}

#[tokio::test]
async fn should_submit_simple() {
    let _ = catapulte::init_logs("debug", false);

    let docker = DockerCli::default();
    let smtp_server = GenericImage::new("rnwood/smtp4dev", "v3")
        .with_wait_for(WaitFor::message_on_stdout(
            "Application started. Press Ctrl+C to shut down.",
        ))
        .with_env_var("ServerOptions__BasePath", "/")
        .with_env_var("ServerOptions__TlsMode", "None")
        .with_exposed_port(25)
        .with_exposed_port(80);

    let smtp_node = docker.run(smtp_server);
    let smtp_port = smtp_node.get_host_port_ipv4(25);

    let app =
        server::Server::from_config(server_config(engine_config(), smtp_config(smtp_port))).app();

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
