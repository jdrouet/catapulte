mod metrics;
mod status;
mod swagger;
mod templates;

use crate::service::provider::TemplateProvider;
use crate::service::render::RenderOptions;
use crate::service::smtp::SmtpPool;
use axum::extract::Extension;
use axum::routing::{get, head, post, Router};
use metrics_exporter_prometheus::PrometheusHandle;
use std::sync::Arc;
use tower_http::trace::TraceLayer;

pub(super) fn create(
    render_options: Arc<RenderOptions>,
    smtp_pool: SmtpPool,
    template_provider: Arc<TemplateProvider>,
    prometheus_handle: Arc<PrometheusHandle>,
) -> Router {
    Router::new()
        .route("/status", head(status::handler))
        .route("/metrics", get(metrics::handler))
        .route("/openapi.json", get(swagger::handler))
        .route("/templates/:name/json", post(templates::json::handler))
        .route(
            "/templates/:name/multipart",
            post(templates::multipart::handler),
        )
        .layer(Extension(render_options))
        .layer(Extension(smtp_pool))
        .layer(Extension(template_provider))
        .layer(Extension(prometheus_handle))
        .layer(TraceLayer::new_for_http())
}
