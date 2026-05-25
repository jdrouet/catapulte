pub mod dto;
pub mod error;
pub mod routes;

use std::net::SocketAddr;

use anyhow::Context;
use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::routing::get;
use axum::routing::post;
use catapulte_domain::use_case::list_emails::ListEmailsUseCase;
use catapulte_domain::use_case::list_events::ListEventsUseCase;
use catapulte_domain::use_case::list_senders::ListSendersUseCase;
use catapulte_domain::use_case::submit_email::SubmitEmailUseCase;
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

pub fn router<S: HttpServerState>(state: S) -> Router {
    Router::new()
        .route(
            "/emails",
            post(crate::routes::emails::submit_email::<S>)
                .get(crate::routes::emails::list_emails::<S>),
        )
        .route(
            "/emails/{id}/events",
            get(crate::routes::events::list_events_for_email::<S>),
        )
        .route("/events", get(crate::routes::events::list_events::<S>))
        .route("/senders", get(crate::routes::senders::list_senders::<S>))
        .route("/health/live", get(crate::routes::health::live))
        .route("/health/ready", get(crate::routes::health::ready))
        .layer(TraceLayer::new_for_http())
        .layer(DefaultBodyLimit::max(crate::dto::MAX_REQUEST_BODY_BYTES))
        .with_state(state)
}

pub struct InboundHttpConfig {
    pub address: SocketAddr,
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
        Ok(Self { address })
    }

    #[must_use]
    pub fn build(self) -> InboundHttpServer {
        InboundHttpServer {
            address: self.address,
        }
    }
}

pub struct InboundHttpServer {
    address: SocketAddr,
}

impl InboundHttpServer {
    /// # Errors
    ///
    /// Returns an error when the listener fails to bind or `axum::serve` exits with an error.
    pub async fn run<S: HttpServerState>(self, state: S) -> anyhow::Result<()> {
        let listener = tokio::net::TcpListener::bind(self.address)
            .await
            .context("binding http listener")?;
        tracing::info!(address = %self.address, "http server listening");
        axum::serve(listener, router(state))
            .await
            .context("http server stopped")?;
        Ok(())
    }
}
