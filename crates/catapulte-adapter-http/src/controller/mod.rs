pub mod metrics;
pub mod status;
pub mod templates;

use std::sync::Arc;

use axum::extract::Extension;
use axum::routing::{Router, get, head, post};
use catapulte_domain::prelude::{EmailSender, TemplateLoader, TemplateRenderer};
use catapulte_domain::service::SendEmailService;
use metrics_exporter_prometheus::PrometheusHandle;
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;

use crate::error::ErrorResponse;

#[derive(OpenApi)]
#[openapi(
    paths(
        templates::json::handler,
    ),
    components(
        schemas(
            templates::json::JsonPayload,
            templates::Recipient,
            ErrorResponse,
        )
    ),
    tags(
        (name = "templates", description = "Template-based email sending endpoints")
    )
)]
struct ApiDoc;

pub fn create_router<L, R, S>(
    service: Arc<SendEmailService<L, R, S>>,
    prometheus_handle: PrometheusHandle,
) -> Router
where
    L: TemplateLoader + 'static,
    R: TemplateRenderer + 'static,
    S: EmailSender + 'static,
{
    Router::new()
        .route("/status", head(status::handler::<L, R, S>))
        .route("/metrics", get(metrics::handler))
        .route(
            "/templates/{name}/json",
            post(templates::json::handler::<L, R, S>),
        )
        .route(
            "/templates/{name}/multipart",
            post(templates::multipart::handler::<L, R, S>),
        )
        .merge(SwaggerUi::new("/swagger-ui").url("/openapi.json", ApiDoc::openapi()))
        .layer(Extension(service))
        .layer(Extension(prometheus_handle))
        .layer(tower_http::trace::TraceLayer::new_for_http())
}
