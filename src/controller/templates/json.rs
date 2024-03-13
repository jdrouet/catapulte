use crate::error::ServerError;
use crate::service::provider::TemplateProvider;
use crate::service::render::RenderService;
use crate::service::smtp::SmtpPool;
use crate::service::template::TemplateOptions;
use axum::extract::{Extension, Json, Path};
use axum::http::StatusCode;
use lettre::Transport;
use serde::Deserialize;
use serde_json::Value as JsonValue;
use utoipa::ToSchema;

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum Recipient {
    One(String),
    More(Vec<String>),
}

impl<'s> utoipa::ToSchema<'s> for Recipient {
    fn schema() -> (
        &'s str,
        utoipa::openapi::RefOr<utoipa::openapi::schema::Schema>,
    ) {
        (
            "Recipient",
            utoipa::openapi::OneOfBuilder::new()
                .item(
                    utoipa::openapi::ObjectBuilder::new()
                        .schema_type(utoipa::openapi::SchemaType::String),
                )
                .item(utoipa::openapi::ArrayBuilder::new().items(
                    utoipa::openapi::Object::with_type(utoipa::openapi::SchemaType::String),
                ))
                .into(),
        )
    }
}

impl Recipient {
    fn into_vec(self) -> Vec<String> {
        match self {
            Recipient::One(item) => vec![item],
            Recipient::More(list) => list,
        }
    }
}

fn option_to_vec(item: Option<Recipient>) -> Vec<String> {
    if let Some(item) = item {
        item.into_vec()
    } else {
        vec![]
    }
}

#[derive(Debug, Deserialize, ToSchema)]
pub(crate) struct JsonPayload {
    pub to: Option<Recipient>,
    pub cc: Option<Recipient>,
    pub bcc: Option<Recipient>,
    pub from: String,
    #[schema(value_type = Object)]
    pub params: JsonValue,
}

impl From<JsonPayload> for TemplateOptions {
    fn from(value: JsonPayload) -> Self {
        let to = option_to_vec(value.to);
        let cc = option_to_vec(value.cc);
        let bcc = option_to_vec(value.bcc);
        TemplateOptions::new(value.from, to, cc, bcc, value.params, Default::default())
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
    Extension(render_service): Extension<RenderService>,
    Extension(smtp_pool): Extension<SmtpPool>,
    Extension(template_provider): Extension<TemplateProvider>,
    Path(name): Path<String>,
    Json(body): Json<JsonPayload>,
) -> Result<StatusCode, ServerError> {
    metrics::counter!("smtp_send", "method" => "json", "template_name" => name.clone())
        .increment(1);
    let template = template_provider.find_by_name(name.as_str()).await?;
    let options: TemplateOptions = body.into();
    options.validate()?;
    let email = template.try_into_email(options, render_service.as_ref())?;
    if let Err(err) = smtp_pool.send(&email) {
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
    use super::{handler, JsonPayload, Recipient};
    use crate::service::smtp::tests::{create_email, expect_latest_inbox};
    use axum::extract::{Extension, Json, Path};
    use axum::http::StatusCode;

    fn create_payload(from: &str, to: &str, token: &str) -> JsonPayload {
        JsonPayload {
            to: Some(Recipient::One(to.to_owned())),
            from: from.to_owned(),
            cc: None,
            bcc: None,
            params: serde_json::json!({
                "name": "bob",
                "token": token,
            }),
        }
    }

    #[tokio::test]
    async fn success() {
        crate::try_init_logs();
        let render_service = crate::service::render::Configuration::default().build();
        let smtp_pool = crate::service::smtp::Configuration::insecure()
            .build()
            .unwrap();
        let template_provider = crate::service::provider::Configuration::default().build();

        let from = create_email();
        let to = create_email();
        let payload = create_payload(&from, &to, "this_is_a_token");

        let result = handler(
            Extension(render_service),
            Extension(smtp_pool),
            Extension(template_provider),
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
        let render_service = crate::service::render::Configuration::default().build();
        let smtp_pool = crate::service::smtp::Configuration::secure()
            .build()
            .unwrap();
        let template_provider = crate::service::provider::Configuration::default().build();

        let from = create_email();
        let to = create_email();
        let payload = create_payload(&from, &to, "this_is_a_secure_token");

        let result = handler(
            Extension(render_service),
            Extension(smtp_pool),
            Extension(template_provider),
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
        let render_service = crate::service::render::Configuration::default().build();
        let smtp_pool = crate::service::smtp::Configuration::insecure()
            .build()
            .unwrap();
        let template_provider = crate::service::provider::Configuration::default().build();

        let from = create_email();
        let to = create_email();
        let mut payload = create_payload(&from, &to, "this_is_a_secure_token");
        payload.params = serde_json::json!({ "name": "Alice" });

        let result = handler(
            Extension(render_service),
            Extension(smtp_pool),
            Extension(template_provider),
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
        let render_service = crate::service::render::Configuration::default().build();
        let smtp_pool = crate::service::smtp::Configuration::insecure()
            .build()
            .unwrap();
        let template_provider = crate::service::provider::Configuration::default().build();
        //
        let from = create_email();
        let to = vec![create_email(), create_email()];
        let cc = vec![create_email(), create_email()];
        let bcc = vec![create_email(), create_email()];
        //
        let payload = JsonPayload {
            to: Some(Recipient::More(to.clone())),
            from: from.to_owned(),
            cc: Some(Recipient::More(cc.clone())),
            bcc: Some(Recipient::More(bcc.clone())),
            params: serde_json::json!({
                "name": "bob",
                "token": "token",
            }),
        };
        //
        let result = handler(
            Extension(render_service),
            Extension(smtp_pool),
            Extension(template_provider),
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
        let render_service = crate::service::render::Configuration::default().build();
        let smtp_pool = crate::service::smtp::Configuration::insecure()
            .build()
            .unwrap();
        let template_provider = crate::service::provider::Configuration::default().build();

        let from = create_email();
        let to = create_email();
        let payload = create_payload(&from, &to, "this_is_a_token");

        let result = handler(
            Extension(render_service),
            Extension(smtp_pool),
            Extension(template_provider),
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
        let render_service = crate::service::render::Configuration::default().build();
        let smtp_pool = crate::service::smtp::Configuration::insecure()
            .build()
            .unwrap();
        let template_provider = crate::service::provider::Configuration::default().build();

        let from = create_email();
        let payload = JsonPayload {
            to: None,
            from: from.to_owned(),
            cc: None,
            bcc: None,
            params: serde_json::json!({}),
        };

        let result = handler(
            Extension(render_service),
            Extension(smtp_pool),
            Extension(template_provider),
            Path("user-login".into()),
            Json(payload),
        )
        .await
        .unwrap_err();
        assert_eq!(result.code, StatusCode::BAD_REQUEST);
        assert_eq!(result.message, "template rendering options invalid");
    }
}
