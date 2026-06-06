pub mod dto;
pub mod error;
pub mod limited_reader;
pub mod routes;

use std::net::SocketAddr;

use anyhow::Context;
use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::routing::get;
use axum::routing::post;
use catapulte_domain::use_case::list_emails::ListEmailsUseCase;
use catapulte_domain::use_case::list_events::ListEventsUseCase;
use catapulte_domain::use_case::list_senders::ListSendersUseCase;
use catapulte_domain::use_case::submit_email::SubmitEmailUseCase;
use tokio_util::sync::CancellationToken;
use tower_http::trace::TraceLayer;

/// Provides the use-case instances that HTTP route handlers dispatch into.
///
/// Implemented by the application state type in the composition root.
pub trait HttpServerState: Clone + Send + Sync + 'static {
    fn submit_email(&self) -> &impl SubmitEmailUseCase;
    fn list_emails(&self) -> &impl ListEmailsUseCase;
    fn list_events(&self) -> &impl ListEventsUseCase;
    fn list_senders(&self) -> &impl ListSendersUseCase;
}

/// Compares two byte slices in constant time to avoid timing side-channels.
///
/// Returns `true` only when both slices have the same length and identical
/// contents. A length mismatch short-circuits before the byte loop, which
/// leaks the length — this is considered acceptable per spec.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Axum middleware that enforces a static bearer-token check.
///
/// Expects `Authorization: Bearer <key>` on every request. Requests that are
/// missing the header, use a scheme other than `Bearer`, or carry a wrong
/// token are rejected with `401 Unauthorized`.
async fn require_api_key(
    axum::extract::State(expected): axum::extract::State<String>,
    request: axum::extract::Request,
    next: Next,
) -> Result<axum::response::Response, StatusCode> {
    let auth_header = request
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .ok_or(StatusCode::UNAUTHORIZED)?;

    let token = auth_header
        .strip_prefix("Bearer ")
        .ok_or(StatusCode::UNAUTHORIZED)?;

    if !constant_time_eq(token.as_bytes(), expected.as_bytes()) {
        return Err(StatusCode::UNAUTHORIZED);
    }

    Ok(next.run(request).await)
}

/// Builds the application router.
///
/// When `api_key` is `Some`, all routes except `/health/live` and
/// `/health/ready` are gated behind `Authorization: Bearer <key>`.
/// When `api_key` is `None`, no authentication is applied.
pub fn router<S: HttpServerState>(state: S, api_key: Option<String>) -> Router {
    let health_routes = Router::new()
        .route("/health/live", get(crate::routes::health::live))
        .route("/health/ready", get(crate::routes::health::ready));

    let protected_routes = Router::new()
        .route(
            "/emails",
            post(crate::routes::emails::submit_email::<S>)
                .get(crate::routes::emails::list_emails::<S>),
        )
        .route(
            "/emails/batch",
            post(crate::routes::emails::submit_email_batch::<S>),
        )
        .route(
            "/emails/{id}/events",
            get(crate::routes::events::list_events_for_email::<S>),
        )
        .route("/events", get(crate::routes::events::list_events::<S>))
        .route("/senders", get(crate::routes::senders::list_senders::<S>))
        .with_state(state);

    let protected_routes = match api_key {
        Some(key) => {
            protected_routes.route_layer(axum::middleware::from_fn_with_state(key, require_api_key))
        }
        None => protected_routes,
    };

    Router::new()
        .merge(protected_routes)
        .merge(health_routes)
        .layer(TraceLayer::new_for_http())
        .layer(DefaultBodyLimit::max(crate::dto::MAX_REQUEST_BODY_BYTES))
}

pub struct InboundHttpConfig {
    pub address: SocketAddr,
    pub api_key: Option<String>,
}

impl InboundHttpConfig {
    /// # Errors
    ///
    /// Returns an error when `<prefix>_ADDRESS` is unset or cannot be parsed as a socket address.
    pub fn from_env(prefix: &str) -> anyhow::Result<Self> {
        let address_key = format!("{prefix}_ADDRESS");
        let raw = std::env::var(&address_key)
            .with_context(|| format!("missing env var {address_key}"))?;
        let address: SocketAddr = raw
            .parse()
            .with_context(|| format!("invalid {address_key}"))?;
        let api_key = std::env::var(format!("{prefix}_API_KEY"))
            .ok()
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty());
        Ok(Self { address, api_key })
    }

    #[must_use]
    pub fn build(self) -> InboundHttpServer {
        InboundHttpServer {
            address: self.address,
            api_key: self.api_key,
        }
    }
}

pub struct InboundHttpServer {
    address: SocketAddr,
    api_key: Option<String>,
}

impl InboundHttpServer {
    /// # Errors
    ///
    /// Returns an error when the listener fails to bind or `axum::serve` exits with an error.
    pub async fn run<S: HttpServerState>(
        self,
        state: S,
        cancel: CancellationToken,
    ) -> anyhow::Result<()> {
        if self.api_key.is_none() {
            tracing::warn!(
                "HTTP API is unauthenticated; set CATAPULTE_HTTP_API_KEY to require a bearer token"
            );
        }
        let listener = tokio::net::TcpListener::bind(self.address)
            .await
            .context("binding http listener")?;
        tracing::info!(address = %self.address, "http server listening");
        axum::serve(listener, router(state, self.api_key))
            .with_graceful_shutdown(async move { cancel.cancelled().await })
            .await
            .context("http server stopped")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::get;
    use tower::ServiceExt;

    use crate::routes::health::{live, ready};

    /// A lightweight router that only includes the health routes with auth applied,
    /// used to test auth in isolation without needing a full `HttpServerState`.
    fn auth_test_router(api_key: Option<String>) -> Router {
        let health_routes = Router::new()
            .route("/health/live", get(live))
            .route("/health/ready", get(ready));

        // Dummy protected route that always returns 200.
        let protected = Router::new().route("/protected", get(|| async { StatusCode::OK }));

        let protected = match api_key {
            Some(key) => protected.route_layer(axum::middleware::from_fn_with_state(
                key,
                crate::require_api_key,
            )),
            None => protected,
        };

        Router::new().merge(protected).merge(health_routes)
    }

    #[tokio::test]
    async fn no_key_configured_protected_route_is_allowed() {
        let app = auth_test_router(None);
        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/protected")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn key_configured_correct_bearer_is_allowed() {
        let app = auth_test_router(Some("secret".to_owned()));
        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/protected")
                    .header("Authorization", "Bearer secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn key_configured_missing_header_returns_401() {
        let app = auth_test_router(Some("secret".to_owned()));
        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/protected")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn key_configured_wrong_key_returns_401() {
        let app = auth_test_router(Some("secret".to_owned()));
        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/protected")
                    .header("Authorization", "Bearer wrongkey")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn key_configured_malformed_header_returns_401() {
        let app = auth_test_router(Some("secret".to_owned()));
        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/protected")
                    .header("Authorization", "Basic secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn health_live_always_public_with_key_configured() {
        let app = auth_test_router(Some("secret".to_owned()));
        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/health/live")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn health_ready_always_public_with_key_configured() {
        let app = auth_test_router(Some("secret".to_owned()));
        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/health/ready")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
}
