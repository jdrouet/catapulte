use crate::error::ServerError;
use crate::service::smtp::SmtpPool;
use axum::extract::{Extension, Json, Path};
use axum::http::StatusCode;
use lettre::message::Mailbox;
use lettre::Transport;
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
    pub from: Mailbox,
    #[schema(value_type = Object)]
    pub params: serde_json::Value,
}

impl JsonPayload {
    fn into_request(self, name: String) -> catapulte_engine::Request {
        catapulte_engine::Request {
            name,
            from: self.from,
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
        (status = 204, description = "Your email has been sent.", body = None),
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
    if let Err(err) = smtp_pool.send(&message) {
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
    use crate::service::smtp::tests::{create_email, expect_latest_inbox};
    use axum::extract::{Extension, Json, Path};
    use axum::http::StatusCode;
    use lettre::message::Mailbox;

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
        let smtp_pool = crate::service::smtp::Configuration::insecure()
            .build()
            .unwrap();
        let engine = catapulte_engine::Config::default().into();

        let from = create_email();
        let to = create_email();
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
        let list = expect_latest_inbox(&from, "to", &to).await;
        let last = list.first().unwrap();
        assert!(last.text.contains("Hello bob!"));
        assert!(last.html.contains("Hello bob!"));
        assert!(last
            .html
            .contains("\"http://example.com/login?token=this_is_a_token\""));
    }

    #[tokio::test]
    async fn success_ssl() {
        crate::try_init_logs();
        let smtp_pool = crate::service::smtp::Configuration::secure()
            .build()
            .unwrap();
        let engine = catapulte_engine::Config::default().into();

        let from = create_email();
        let to = create_email();
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
        let list = expect_latest_inbox(&from, "to", &to).await;
        let last = list.first().unwrap();
        assert!(last.text.contains("Hello bob!"));
        assert!(last.html.contains("Hello bob!"));
        assert!(last
            .html
            .contains("\"http://example.com/login?token=this_is_a_secure_token\""));
    }

    #[tokio::test]
    async fn success_even_missing_params() {
        let smtp_pool = crate::service::smtp::Configuration::insecure()
            .build()
            .unwrap();
        let engine = catapulte_engine::Config::default().into();

        let from = create_email();
        let to = create_email();
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
        let list = expect_latest_inbox(&from, "to", &to).await;
        let last = list.first().unwrap();
        assert!(last.text.contains("Hello Alice!"));
        assert!(last.html.contains("Hello Alice!"));
        assert!(last.html.contains("\"http://example.com/login?token=\""));
    }

    #[tokio::test]
    async fn success_multiple_recipients() {
        let smtp_pool = crate::service::smtp::Configuration::insecure()
            .build()
            .unwrap();
        let engine = catapulte_engine::Config::default().into();
        //
        let from = create_email();
        let to = vec![create_email(), create_email()];
        let cc = vec![create_email(), create_email()];
        let bcc = vec![create_email(), create_email()];
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
        for (kind, email) in to
            .iter()
            .map(|email| ("to", email))
            .chain(cc.iter().map(|email| ("cc", email)))
            .chain(bcc.iter().map(|email| ("bcc", email)))
        {
            let list = expect_latest_inbox(&from, kind, email).await;
            let last = list.first().unwrap();
            assert!(last.text.contains("Hello bob!"));
            assert!(last.html.contains("Hello bob!"));
        }
    }

    #[tokio::test]
    async fn failure_template_not_found() {
        crate::try_init_logs();
        let smtp_pool = crate::service::smtp::Configuration::insecure()
            .build()
            .unwrap();
        let engine = catapulte_engine::Config::default().into();

        let from = create_email();
        let to = create_email();
        let payload = create_payload(&from, &to, "this_is_a_token");

        let result = handler(
            Extension(smtp_pool),
            Extension(engine),
            Path("not-found".into()),
            Json(payload),
        )
        .await
        .unwrap_err();
        assert_eq!(result.code, StatusCode::BAD_REQUEST);
        assert_eq!(result.message, "unable to prepare template");
    }

    #[tokio::test]
    async fn failure_no_recipient() {
        crate::try_init_logs();
        let smtp_pool = crate::service::smtp::Configuration::insecure()
            .build()
            .unwrap();
        let engine = catapulte_engine::Config::default().into();

        let from = create_email();
        let payload = JsonPayload {
            to: Recipient::default(),
            from: from.clone(),
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
        assert_eq!(result.code, StatusCode::BAD_REQUEST);
        assert_eq!(result.message, "template rendering options invalid");
    }
}
