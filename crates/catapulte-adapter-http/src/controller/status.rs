use std::sync::Arc;
use std::time::Duration;

use axum::extract::Extension;
use axum::http::StatusCode;
use catapulte_domain::prelude::{EmailSender, TemplateLoader, TemplateRenderer};
use catapulte_domain::service::SendEmailService;

use crate::error::ErrorResponse;

/// Check the status of Catapulte
///
/// Just answers if everything is going fine.
#[utoipa::path(
    operation_id = "status",
    head,
    path = "/status",
    responses(
        (status = 204, description = "Everything is running smoothly."),
        (status = 500, description = "The SMTP server cannot be reached."),
    )
)]
pub async fn handler<L, R, S>(
    Extension(service): Extension<Arc<SendEmailService<L, R, S>>>,
) -> Result<StatusCode, ErrorResponse>
where
    L: TemplateLoader + 'static,
    R: TemplateRenderer + 'static,
    S: EmailSender + 'static,
{
    metrics::counter!("status_check").increment(1);

    let future = service.test_connection();
    match tokio::time::timeout(Duration::from_secs(1), future).await {
        Ok(Ok(())) => Ok(StatusCode::NO_CONTENT),
        Ok(Err(err)) => Err(err.into()),
        Err(_elapsed) => Err(ErrorResponse {
            status: StatusCode::GATEWAY_TIMEOUT,
            code: "connection-timeout",
            title: "connection to email server timed out",
            details: Vec::new(),
        }),
    }
}
