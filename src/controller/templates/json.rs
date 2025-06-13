use crate::error::ServerError;
use crate::service::smtp::SmtpPool;
use axum::extract::{Extension, Json, Path};
use axum::http::StatusCode;
use lettre::AsyncTransport;
use serde::Deserialize;
use utoipa::ToSchema;

#[derive(Debug, Deserialize, ToSchema)]
pub(crate) struct JsonPayload {
    #[serde(default)]
    pub to: super::Recipient,
    #[serde(default)]
    pub cc: super::Recipient,
    #[serde(default)]
    pub bcc: super::Recipient,
    pub from: super::Mailbox,
    #[schema(value_type = Object)]
    pub params: serde_json::Value,
}

impl JsonPayload {
    fn into_request(self, name: String) -> catapulte_engine::Request {
        catapulte_engine::Request {
            name,
            from: self.from.inner(),
            to: self.to.into_vec(),
            cc: self.cc.into_vec(),
            bcc: self.bcc.into_vec(),
            params: self.params,
            attachments: Default::default(),
        }
    }
}

#[utoipa::path(
    operation_id = "send_json",
    post,
    path = "/templates/{name}/json",
    params(
        ("name" = String, Path, description = "Name of the template.")
    ),
    request_body(content = JsonPayload, content_type = "application/json"),
    responses(
        (status = 204, description = "Your email has been sent.", body = ()),
    )
)]
pub(crate) async fn handler(
    Extension(smtp_pool): Extension<SmtpPool>,
    Extension(engine): Extension<catapulte_engine::Engine>,
    Path(name): Path<String>,
    Json(body): Json<JsonPayload>,
) -> Result<StatusCode, ServerError> {
    let req = body.into_request(name.clone());
    let message = engine.handle(req).await?;

    metrics::counter!("smtp_send", "method" => "json", "template_name" => name.clone())
        .increment(1);
    if let Err(err) = smtp_pool.send(message).await {
        metrics::counter!("smtp_send_error", "method" => "json", "template_name" => name)
            .increment(1);
        Err(err)?
    } else {
        metrics::counter!("smtp_send_success", "method" => "json", "template_name" => name)
            .increment(1);
        Ok(StatusCode::NO_CONTENT)
    }
}

#[cfg(test)]
mod tests {
    use super::super::Recipient;
    use super::{handler, JsonPayload};
    use crate::controller::templates::Mailbox;
    use crate::error::ServerError;
    use crate::service::smtp::tests::{
        create_email, smtp_image_insecure, smtp_image_secure, SmtpMock, HTTP_PORT, SMTP_PORT,
    };
    use axum::extract::{Extension, Json, Path};
    use axum::http::StatusCode;
    use testcontainers::runners::AsyncRunner;

    fn create_payload(from: &Mailbox, to: &Mailbox, token: &str) -> JsonPayload {
        JsonPayload {
            from: from.clone(),
            to: Recipient::One(to.clone()),
            cc: Recipient::default(),
            bcc: Recipient::default(),
            params: serde_json::json!({
                "name": "bob",
                "token": token,
            }),
        }
    }

    #[tokio::test]
    async fn success() {
        crate::try_init_logs();

        let smtp_node = smtp_image_insecure().start().await.unwrap();
        let smtp_port = smtp_node.get_host_port_ipv4(SMTP_PORT).await.unwrap();
        let http_port = smtp_node.get_host_port_ipv4(HTTP_PORT).await.unwrap();

        let smtp_mock = SmtpMock::new("localhost", http_port);

        let smtp_pool = crate::service::smtp::Configuration::insecure(smtp_port)
            .build()
            .unwrap();
        let engine = catapulte_engine::Config::default().into();

        let from = Mailbox(create_email());
        let to = Mailbox(create_email());
        let payload = create_payload(&from, &to, "this_is_a_token");

        let result = handler(
            Extension(smtp_pool),
            Extension(engine),
            Path("user-login".into()),
            Json(payload),
        )
        .await
        .unwrap();
        assert_eq!(result, StatusCode::NO_CONTENT);
        //
        let messages = smtp_mock.expect_latest_inbox().await;
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].inner.subject, "Hello bob!");
        assert_eq!(messages[0].inner.from, from.0.email.to_string());
        assert!(messages[0]
            .inner
            .to
            .iter()
            .any(|addr| addr.email.eq(&to.0.email)));
        let msg = messages[0].detailed().await;
        let text = msg.plaintext().await;
        assert!(text.contains("Hello bob!"));
        let html = msg.html().await;
        assert!(html.contains("Hello bob!"));
        assert!(html.contains("\"http://example.com/login?token=this_is_a_token\""));
    }

    #[tokio::test]
    async fn success_ssl() {
        crate::try_init_logs();

        let smtp_node = smtp_image_secure().start().await.unwrap();
        let smtp_port = smtp_node.get_host_port_ipv4(SMTP_PORT).await.unwrap();
        let http_port = smtp_node.get_host_port_ipv4(HTTP_PORT).await.unwrap();

        let smtp_mock = SmtpMock::new("localhost", http_port);

        let smtp_pool = crate::service::smtp::Configuration::secure(smtp_port)
            .build()
            .unwrap();
        let engine = catapulte_engine::Config::default().into();

        let from = Mailbox(create_email());
        let to = Mailbox(create_email());
        let payload = create_payload(&from, &to, "this_is_a_secure_token");

        let result = handler(
            Extension(smtp_pool),
            Extension(engine),
            Path("user-login".into()),
            Json(payload),
        )
        .await
        .unwrap();
        assert_eq!(result, StatusCode::NO_CONTENT);
        //
        let messages = smtp_mock.expect_latest_inbox().await;
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].inner.subject, "Hello bob!");
        assert_eq!(messages[0].inner.from, from.0.email.to_string());
        assert!(messages[0]
            .inner
            .to
            .iter()
            .any(|addr| addr.email.eq(&to.0.email)));
        let msg = messages[0].detailed().await;
        let text = msg.plaintext().await;
        assert!(text.contains("Hello bob!"));
        let html = msg.html().await;
        assert!(html.contains("Hello bob!"));
        assert!(html.contains("\"http://example.com/login?token=this_is_a_secure_token\""));
    }

    #[tokio::test]
    async fn success_even_missing_params() {
        let smtp_node = smtp_image_insecure().start().await.unwrap();
        let smtp_port = smtp_node.get_host_port_ipv4(SMTP_PORT).await.unwrap();
        let http_port = smtp_node.get_host_port_ipv4(HTTP_PORT).await.unwrap();

        let smtp_mock = SmtpMock::new("localhost", http_port);

        let smtp_pool = crate::service::smtp::Configuration::insecure(smtp_port)
            .build()
            .unwrap();
        let engine = catapulte_engine::Config::default().into();

        let from = Mailbox(create_email());
        let to = Mailbox(create_email());
        let mut payload = create_payload(&from, &to, "this_is_a_secure_token");
        payload.params = serde_json::json!({ "name": "Alice" });

        let result = handler(
            Extension(smtp_pool),
            Extension(engine),
            Path("user-login".into()),
            Json(payload),
        )
        .await
        .unwrap();
        assert_eq!(result, StatusCode::NO_CONTENT);

        let messages = smtp_mock.expect_latest_inbox().await;
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].inner.subject, "Hello Alice!");
        assert_eq!(messages[0].inner.from, from.0.email.to_string());
        assert!(messages[0]
            .inner
            .to
            .iter()
            .any(|addr| addr.email.eq(&to.0.email)));
    }

    #[tokio::test]
    async fn success_multiple_recipients() {
        let smtp_node = smtp_image_insecure().start().await.unwrap();
        let smtp_port = smtp_node.get_host_port_ipv4(SMTP_PORT).await.unwrap();
        let http_port = smtp_node.get_host_port_ipv4(HTTP_PORT).await.unwrap();

        let smtp_mock = SmtpMock::new("localhost", http_port);

        let smtp_pool = crate::service::smtp::Configuration::insecure(smtp_port)
            .build()
            .unwrap();
        let engine = catapulte_engine::Config::default().into();
        //
        let from = Mailbox(create_email());
        let to = vec![Mailbox(create_email()), Mailbox(create_email())];
        let cc = vec![Mailbox(create_email()), Mailbox(create_email())];
        let bcc = vec![Mailbox(create_email()), Mailbox(create_email())];
        //
        let payload = JsonPayload {
            to: Recipient::Many(to.clone()),
            from: from.to_owned(),
            cc: Recipient::Many(cc.clone()),
            bcc: Recipient::Many(bcc.clone()),
            params: serde_json::json!({
                "name": "bob",
                "token": "token",
            }),
        };
        //
        let result = handler(
            Extension(smtp_pool),
            Extension(engine),
            Path("user-login".into()),
            Json(payload),
        )
        .await
        .unwrap();
        assert_eq!(result, StatusCode::NO_CONTENT);

        let messages = smtp_mock.expect_latest_inbox().await;
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].inner.subject, "Hello bob!");
        assert_eq!(messages[0].inner.from, from.0.email.to_string());
        assert_eq!(messages[0].inner.to.iter().count(), 6)
    }

    #[tokio::test]
    async fn failure_template_not_found() {
        crate::try_init_logs();

        let smtp_node = smtp_image_insecure().start().await.unwrap();
        let smtp_port = smtp_node.get_host_port_ipv4(SMTP_PORT).await.unwrap();

        let smtp_pool = crate::service::smtp::Configuration::insecure(smtp_port)
            .build()
            .unwrap();
        let engine = catapulte_engine::Config::default().into();

        let from = Mailbox(create_email());
        let to = Mailbox(create_email());
        let payload = create_payload(&from, &to, "this_is_a_token");

        let result = handler(
            Extension(smtp_pool),
            Extension(engine),
            Path("not-found".into()),
            Json(payload),
        )
        .await
        .unwrap_err();
        assert!(matches!(result, ServerError::Engine(_)));
    }

    #[tokio::test]
    async fn failure_no_recipient() {
        crate::try_init_logs();

        let smtp_node = smtp_image_insecure().start().await.unwrap();
        let smtp_port = smtp_node.get_host_port_ipv4(SMTP_PORT).await.unwrap();

        let smtp_pool = crate::service::smtp::Configuration::insecure(smtp_port)
            .build()
            .unwrap();
        let engine = catapulte_engine::Config::default().into();

        let from = create_email();
        let payload = JsonPayload {
            to: Recipient::default(),
            from: Mailbox(from.clone()),
            cc: Recipient::default(),
            bcc: Recipient::default(),
            params: serde_json::json!({}),
        };

        let result = handler(
            Extension(smtp_pool),
            Extension(engine),
            Path("user-login".into()),
            Json(payload),
        )
        .await
        .unwrap_err();
        assert!(matches!(result, ServerError::Engine(_)));
    }
}
