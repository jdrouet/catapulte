use axum::extract::Extension;
use metrics_exporter_prometheus::PrometheusHandle;
use std::sync::Arc;

/// Check the metrics of the service
///
/// Returns the metrics reported by the service using the prometheus format.
#[utoipa::path(
    operation_id = "metrics",
    get,
    path = "/metrics",
    responses(
        (
            status = 200,
            description = "Prometheus format of the metrics of the service.",
            body = String,
            example = json!("# TYPE status_check counter\nstatus_check 1\n"),
        ),
    )
)]
pub(super) async fn handler(Extension(handle): Extension<Arc<PrometheusHandle>>) -> String {
    handle.render()
}
