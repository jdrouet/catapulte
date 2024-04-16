use std::{net::Ipv4Addr, path::PathBuf};

use catapulte::service::{server, smtp};

use axum::{
    body::Body,
    http::{header, Request, StatusCode},
};
// use http_body_util::BodyExt; // for `collect`
use tower::ServiceExt; // for `call`, `oneshot`, and `ready`

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

fn smtp_config() -> smtp::Configuration {
    smtp::Configuration {
        hostname: "localhost".into(),
        port: 1025,
        ..Default::default()
    }
}

fn server_config() -> server::Configuration {
    server::Configuration {
        host: Ipv4Addr::new(127, 0, 0, 1).into(),
        port: 3000,
        engine: engine_config(),
        smtp: smtp_config(),
    }
}

#[tokio::test]
async fn should_submit_simple() {
    let _ = catapulte::init_logs("trace", false);
    let app = server::Server::from_config(server_config()).app();

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
