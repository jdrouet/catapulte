use std::sync::Arc;

use axum::extract::{Extension, Json, Path};
use axum::http::StatusCode;
use catapulte_domain::model::{Email, Recipients};
use catapulte_domain::prelude::{EmailSender, TemplateLoader, TemplateRenderer};
use catapulte_domain::service::SendEmailService;
use serde::Deserialize;
use utoipa::ToSchema;

use super::{Mailbox, Recipient};
use crate::error::ErrorResponse;

#[derive(Debug, Deserialize, ToSchema)]
pub struct JsonPayload {
    #[serde(default)]
    pub to: Recipient,
    #[serde(default)]
    pub cc: Recipient,
    #[serde(default)]
    pub bcc: Recipient,
    pub from: Mailbox,
    #[schema(value_type = Object)]
    pub params: serde_json::Value,
}

impl JsonPayload {
    fn into_email(self, template_name: String) -> Result<Email, ErrorResponse> {
        let from = self.from.into_domain()?;
        let to = self.to.into_domain_vec()?;
        let cc = self.cc.into_domain_vec()?;
        let bcc = self.bcc.into_domain_vec()?;

        Ok(Email {
            template_name,
            from,
            recipients: Recipients { to, cc, bcc },
            params: self.params,
            attachments: Vec::new(),
        })
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
pub async fn handler<L, R, S>(
    Extension(service): Extension<Arc<SendEmailService<L, R, S>>>,
    Path(name): Path<String>,
    Json(body): Json<JsonPayload>,
) -> Result<StatusCode, ErrorResponse>
where
    L: TemplateLoader + 'static,
    R: TemplateRenderer + 'static,
    S: EmailSender + 'static,
{
    let email = body.into_email(name.clone())?;

    metrics::counter!("smtp_send", "method" => "json", "template_name" => name.clone())
        .increment(1);

    match service.send(&email).await {
        Ok(()) => {
            metrics::counter!("smtp_send_success", "method" => "json", "template_name" => name)
                .increment(1);
            Ok(StatusCode::NO_CONTENT)
        }
        Err(err) => {
            metrics::counter!("smtp_send_error", "method" => "json", "template_name" => name)
                .increment(1);
            Err(err.into())
        }
    }
}
