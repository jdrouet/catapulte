pub(crate) mod metrics;
pub(crate) mod status;
pub(crate) mod swagger;
pub(crate) mod templates;

use axum::routing::{get, head, post, Router};

pub(crate) fn create() -> Router {
    Router::new()
        .route("/status", head(status::handler))
        .route("/metrics", get(metrics::handler))
        .route("/templates/:name/json", post(templates::json::handler))
        .route(
            "/templates/:name/multipart",
            post(templates::multipart::handler),
        )
        .merge(swagger::service())
}
