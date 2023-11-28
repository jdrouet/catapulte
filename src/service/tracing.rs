use tower_http::trace::MakeSpan;
use tracing::{Level, Span};

#[derive(Clone, Debug, Default, serde::Deserialize)]
pub struct Configuration {
    pub(crate) header: Option<String>,
}

impl Configuration {
    pub fn add_layer(&self, router: axum::Router) -> axum::Router {
        if let Some(ref header) = self.header {
            router.layer(
                tower_http::trace::TraceLayer::new_for_http()
                    .make_span_with(WithTraceIdMakeSpan::new(header.clone())),
            )
        } else {
            router.layer(tower_http::trace::TraceLayer::new_for_http())
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct WithTraceIdMakeSpan {
    header: String,
}

impl WithTraceIdMakeSpan {
    pub fn new(header: String) -> Self {
        Self { header }
    }
}

impl<B> MakeSpan<B> for WithTraceIdMakeSpan {
    fn make_span(&mut self, request: &axum::http::Request<B>) -> Span {
        let trace_id = request
            .headers()
            .get(&self.header)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");

        tracing::span!(
            // TODO add a way to change this level
            Level::DEBUG,
            "request",
            method = %request.method(),
            uri = ?request.uri().to_string(),
            version = ?request.version(),
            trace_id = ?trace_id,
        )
    }
}
