pub mod auth;
pub mod dto;
pub mod error;
pub mod feedback;
pub mod limited_reader;
pub mod routes;

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Context;
use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::routing::get;
use axum::routing::post;
use catapulte_domain::use_case::ingest_feedback::IngestFeedbackUseCase;
use catapulte_domain::use_case::list_emails::ListEmailsUseCase;
use catapulte_domain::use_case::list_events::ListEventsUseCase;
use catapulte_domain::use_case::list_senders::ListSendersUseCase;
use catapulte_domain::use_case::manage_suppressions::ManageSuppressionsUseCase;
use catapulte_domain::use_case::manage_templates::ManageTemplatesUseCase;
use catapulte_domain::use_case::submit_email::SubmitEmailUseCase;
use tokio_util::sync::CancellationToken;

use crate::auth::{AuthConfig, require_api_key, require_feedback_secret, trust_every_caller};
use tower_http::trace::TraceLayer;

/// Provides the readiness check that the `/health/ready` route dispatches into.
///
/// Implemented by the application state type in the composition root.
pub trait ReadinessState: Clone + Send + Sync + 'static {
    fn check_readiness(
        &self,
    ) -> &impl catapulte_domain::use_case::check_readiness::CheckReadinessUseCase;
}

/// Provides the use-case instances that HTTP route handlers dispatch into.
///
/// Implemented by the application state type in the composition root.
pub trait HttpServerState: ReadinessState {
    fn submit_email(&self) -> &impl SubmitEmailUseCase;
    fn list_emails(&self) -> &impl ListEmailsUseCase;
    fn list_events(&self) -> &impl ListEventsUseCase;
    fn list_senders(&self) -> &impl ListSendersUseCase;
    fn manage_suppressions(&self) -> &impl ManageSuppressionsUseCase;
    fn ingest_feedback(&self) -> &impl IngestFeedbackUseCase;
    fn manage_templates(&self) -> &impl ManageTemplatesUseCase;
}

/// Builds the application router.
///
/// When `auth` is `Some`, all routes except `/health/live` and `/health/ready` need
/// `Authorization: Bearer <secret>` matching one of its keys. When `auth` is `None`, no
/// authentication is applied and every caller is the trusted administrator.
pub fn router<S: HttpServerState>(
    state: S,
    auth: Option<Arc<AuthConfig>>,
    request_timeout: std::time::Duration,
) -> Router {
    router_with_feedback(state, auth, None, request_timeout)
}

/// Largest report body `POST /inbound/feedback` reads.
pub const MAX_FEEDBACK_BYTES: usize = 1024 * 1024;

/// Like [`router`], and when `feedback_secret` is set also serves `POST /inbound/feedback`,
/// guarded by that secret alone and outside the API-key layer.
pub fn router_with_feedback<S: HttpServerState>(
    state: S,
    auth: Option<Arc<AuthConfig>>,
    feedback_secret: Option<Arc<str>>,
    request_timeout: std::time::Duration,
) -> Router {
    let feedback_routes = match feedback_secret {
        Some(secret) => Router::new()
            .route(
                "/inbound/feedback",
                post(crate::routes::feedback::ingest_feedback::<S>),
            )
            .route_layer(axum::middleware::from_fn_with_state(
                secret,
                require_feedback_secret,
            ))
            .layer(DefaultBodyLimit::max(MAX_FEEDBACK_BYTES))
            .with_state(state.clone()),
        None => Router::new(),
    };
    // Bounds slow/hung requests. Deliberately NOT applied to the submit routes
    // below: those stream multipart attachment bodies (up to several hundred MiB)
    // and a whole-request deadline would truncate legitimate large uploads over
    // slow links. Submit is instead bounded by the body-size limit and the
    // per-attachment fetch timeouts. Reads and the health probes are bounded here.
    let timeout_layer = tower_http::timeout::TimeoutLayer::with_status_code(
        axum::http::StatusCode::REQUEST_TIMEOUT,
        request_timeout,
    );

    let health_routes = Router::new()
        .route("/health/live", get(crate::routes::health::live))
        .route("/health/ready", get(crate::routes::health::ready::<S>))
        .layer(timeout_layer)
        .with_state(state.clone());

    let read_routes = Router::new()
        .route("/emails", get(crate::routes::emails::list_emails::<S>))
        .route(
            "/emails/{id}/events",
            get(crate::routes::events::list_events_for_email::<S>),
        )
        .route("/events", get(crate::routes::events::list_events::<S>))
        .route("/senders", get(crate::routes::senders::list_senders::<S>))
        .layer(timeout_layer);

    let suppression_routes = Router::new()
        .route(
            "/suppressions",
            get(crate::routes::suppressions::list_suppressions::<S>)
                .post(crate::routes::suppressions::add_suppressions::<S>),
        )
        .route(
            "/suppressions/{address}",
            axum::routing::delete(crate::routes::suppressions::delete_suppression::<S>),
        )
        .layer(timeout_layer);

    let template_routes = Router::new()
        .route(
            "/templates",
            get(crate::routes::templates::list_templates::<S>),
        )
        .route(
            "/templates/{name}",
            get(crate::routes::templates::get_template::<S>)
                .put(crate::routes::templates::put_template::<S>)
                .delete(crate::routes::templates::delete_template::<S>),
        )
        .route(
            "/templates/{name}/preview",
            post(crate::routes::templates::preview_template::<S>),
        )
        .layer(DefaultBodyLimit::max(
            crate::routes::templates::MAX_TEMPLATE_REQUEST_BYTES,
        ))
        .layer(timeout_layer);

    let submit_routes = Router::new()
        .route("/emails", post(crate::routes::emails::submit_email::<S>))
        .route(
            "/emails/batch",
            post(crate::routes::emails::submit_email_batch::<S>),
        );

    let protected_routes = read_routes
        .merge(suppression_routes)
        .merge(template_routes)
        .merge(submit_routes)
        .with_state(state);

    let protected_routes = match auth {
        Some(auth) => protected_routes
            .route_layer(axum::middleware::from_fn_with_state(auth, require_api_key)),
        None => protected_routes.route_layer(axum::middleware::from_fn(trust_every_caller)),
    };

    Router::new()
        .merge(protected_routes)
        .merge(feedback_routes)
        .merge(health_routes)
        .layer(TraceLayer::new_for_http())
        .layer(DefaultBodyLimit::max(crate::dto::MAX_REQUEST_BODY_BYTES))
}

pub struct InboundHttpConfig {
    pub address: SocketAddr,
    pub auth: Option<Arc<AuthConfig>>,
    /// Enables `POST /inbound/feedback`; `None` leaves the route out.
    pub feedback_secret: Option<Arc<str>>,
    pub request_timeout: std::time::Duration,
}

/// Shortest accepted feedback secret, in bytes.
const MIN_FEEDBACK_SECRET_BYTES: usize = 32;

fn feedback_secret_from_env() -> anyhow::Result<Option<Arc<str>>> {
    const KEY: &str = "CATAPULTE_BOUNCE_INGEST_SECRET";
    let Some(raw) = crate::auth::optional_var(&|k: &str| std::env::var(k), KEY)? else {
        return Ok(None);
    };
    let secret = raw.trim();
    if secret.is_empty() {
        return Ok(None);
    }
    anyhow::ensure!(
        secret.len() >= MIN_FEEDBACK_SECRET_BYTES,
        "{KEY} must be at least {MIN_FEEDBACK_SECRET_BYTES} bytes (for example `openssl rand -base64 48`)"
    );
    Ok(Some(Arc::from(secret)))
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
        let auth = AuthConfig::from_lookup(&format!("{prefix}_API_KEY"), |key| std::env::var(key))
            .context("loading API keys")?
            .map(Arc::new);
        let feedback_secret = feedback_secret_from_env()?;
        if let (Some(secret), Some(auth)) = (&feedback_secret, &auth) {
            anyhow::ensure!(
                !auth.has_secret(secret),
                "CATAPULTE_BOUNCE_INGEST_SECRET must differ from every API key secret"
            );
        }
        let timeout_key = format!("{prefix}_REQUEST_TIMEOUT_SECS");
        let request_timeout_secs: u64 = match std::env::var(&timeout_key) {
            Err(std::env::VarError::NotPresent) => 30,
            Err(e) => return Err(anyhow::Error::new(e).context(format!("reading {timeout_key}"))),
            Ok(v) => v
                .parse()
                .with_context(|| format!("invalid {timeout_key}: {v:?}"))?,
        };
        let request_timeout = std::time::Duration::from_secs(request_timeout_secs);
        Ok(Self {
            address,
            auth,
            feedback_secret,
            request_timeout,
        })
    }

    #[must_use]
    pub fn build(self) -> InboundHttpServer {
        InboundHttpServer {
            address: self.address,
            auth: self.auth,
            feedback_secret: self.feedback_secret,
            request_timeout: self.request_timeout,
        }
    }
}

pub struct InboundHttpServer {
    address: SocketAddr,
    auth: Option<Arc<AuthConfig>>,
    feedback_secret: Option<Arc<str>>,
    request_timeout: std::time::Duration,
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
        if self.auth.is_none() {
            tracing::warn!(
                "HTTP API is unauthenticated and every caller is a trusted administrator; \
                 set CATAPULTE_HTTP_API_KEY or CATAPULTE_API_KEYS to require a bearer token"
            );
        }
        let listener = tokio::net::TcpListener::bind(self.address)
            .await
            .context("binding http listener")?;
        tracing::info!(address = %self.address, "http server listening");
        axum::serve(
            listener,
            router_with_feedback(state, self.auth, self.feedback_secret, self.request_timeout),
        )
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
    use catapulte_domain::use_case::check_readiness::Readiness;
    use std::sync::Arc;
    use tower::ServiceExt;

    use crate::auth::{ApiKeyIdentity, AuthConfig, NamedApiKey};
    use crate::routes::health::live;

    #[derive(Clone)]
    struct M;

    impl crate::ReadinessState for M {
        fn check_readiness(
            &self,
        ) -> &impl catapulte_domain::use_case::check_readiness::CheckReadinessUseCase {
            self
        }
    }

    impl catapulte_domain::use_case::check_readiness::CheckReadinessUseCase for M {
        async fn check_readiness(&self) -> Readiness {
            Readiness::Ready
        }
    }

    fn legacy(secret: &str) -> Option<Arc<AuthConfig>> {
        AuthConfig::new(Some(secret.to_owned()), vec![])
            .unwrap()
            .map(Arc::new)
    }

    /// A lightweight router that only includes the health routes with auth applied,
    /// used to test auth in isolation without needing a full `HttpServerState`. The
    /// protected route echoes the caller's identity.
    fn auth_test_router(auth: Option<Arc<AuthConfig>>) -> Router {
        let mock = M;
        let health_routes = Router::new()
            .route("/health/live", get(live))
            .route("/health/ready", get(crate::routes::health::ready::<M>))
            .with_state(mock);

        let protected = Router::new().route(
            "/protected",
            get(
                |axum::Extension(id): axum::Extension<ApiKeyIdentity>| async move {
                    format!("{}|{}", id.owner, id.admin)
                },
            ),
        );

        let protected = match auth {
            Some(auth) => protected.route_layer(axum::middleware::from_fn_with_state(
                auth,
                crate::auth::require_api_key,
            )),
            None => {
                protected.route_layer(axum::middleware::from_fn(crate::auth::trust_every_caller))
            }
        };

        Router::new().merge(protected).merge(health_routes)
    }

    async fn identity_of(app: Router, token: Option<&str>) -> Result<String, StatusCode> {
        let mut request = Request::builder().method("GET").uri("/protected");
        if let Some(token) = token {
            request = request.header("Authorization", format!("Bearer {token}"));
        }
        let response = app
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        if response.status() != StatusCode::OK {
            return Err(response.status());
        }
        let bytes = http_body_util::BodyExt::collect(response.into_body())
            .await
            .unwrap()
            .to_bytes();
        Ok(String::from_utf8(bytes.to_vec()).unwrap())
    }

    fn two_keys() -> Option<Arc<AuthConfig>> {
        AuthConfig::new(
            None,
            vec![
                NamedApiKey {
                    name: "a".into(),
                    secret: "secret-a".into(),
                    admin: false,
                    sender_domains: None,
                },
                NamedApiKey {
                    name: "ops".into(),
                    secret: "secret-ops".into(),
                    admin: true,
                    sender_domains: None,
                },
            ],
        )
        .unwrap()
        .map(Arc::new)
    }

    #[tokio::test]
    async fn identities_follow_the_key_used() {
        assert_eq!(
            identity_of(auth_test_router(None), None).await.unwrap(),
            "|true"
        );
        assert_eq!(
            identity_of(auth_test_router(legacy("secret")), Some("secret"))
                .await
                .unwrap(),
            "|true"
        );
        assert_eq!(
            identity_of(auth_test_router(two_keys()), Some("secret-a"))
                .await
                .unwrap(),
            "a|false"
        );
        assert_eq!(
            identity_of(auth_test_router(two_keys()), Some("secret-ops"))
                .await
                .unwrap(),
            "ops|true"
        );
    }

    #[test]
    fn has_secret_finds_legacy_and_named_secrets_only() {
        let keys = two_keys().unwrap();
        assert!(keys.has_secret("secret-a") && keys.has_secret("secret-ops"));
        assert!(!keys.has_secret("secret-b"));
        assert!(legacy("legacy").unwrap().has_secret("legacy"));
    }

    #[tokio::test]
    async fn named_keys_alone_still_enforce_auth() {
        assert_eq!(
            identity_of(auth_test_router(two_keys()), None).await,
            Err(StatusCode::UNAUTHORIZED)
        );
        assert_eq!(
            identity_of(auth_test_router(two_keys()), Some("nope")).await,
            Err(StatusCode::UNAUTHORIZED)
        );
    }

    #[test]
    fn duplicate_secrets_are_rejected() {
        let key = |name: &str, secret: &str| NamedApiKey {
            name: name.into(),
            secret: secret.into(),
            admin: false,
            sender_domains: None,
        };
        assert!(AuthConfig::new(None, vec![key("a", "s"), key("b", "s")]).is_err());
        assert!(AuthConfig::new(Some("s".into()), vec![key("a", "s")]).is_err());
        assert!(AuthConfig::new(None, vec![key("a", "s1"), key("a", "s2")]).is_err());
        assert!(AuthConfig::new(None, vec![key("A-b", "s")]).is_err());
        assert!(AuthConfig::new(None, vec![key("", "s")]).is_err());
    }

    #[test]
    fn named_keys_are_read_from_the_environment() {
        let vars: std::collections::HashMap<&str, &str> = [
            ("CATAPULTE_API_KEYS", "ci, ops"),
            ("CATAPULTE_API_KEY_CI_SECRET", "s1"),
            ("CATAPULTE_API_KEY_CI_SENDER_DOMAINS", "Example.com"),
            ("CATAPULTE_API_KEY_OPS_SECRET", "s2"),
            ("CATAPULTE_API_KEY_OPS_ADMIN", "true"),
        ]
        .into();
        let lookup = |k: &str| {
            vars.get(k)
                .map(|v| (*v).to_owned())
                .ok_or(std::env::VarError::NotPresent)
        };

        let auth = AuthConfig::from_lookup("LEGACY", lookup).unwrap().unwrap();

        let ci = auth.authenticate("s1").unwrap();
        assert_eq!(ci.owner, "ci");
        assert!(!ci.admin);
        assert_eq!(
            ci.sender_domains,
            Some(["example.com".to_owned()].into_iter().collect())
        );
        assert!(auth.authenticate("s2").unwrap().admin);
        assert!(auth.authenticate("s3").is_none());
        assert!(
            AuthConfig::from_lookup("LEGACY", |_| Err(std::env::VarError::NotPresent))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn a_value_that_is_not_unicode_fails_startup_naming_the_variable() {
        let bad = || std::env::VarError::NotUnicode(std::ffi::OsString::new());
        let present: std::collections::HashMap<&str, &str> = [
            ("CATAPULTE_API_KEYS", "ci"),
            ("CATAPULTE_API_KEY_CI_SECRET", "s"),
        ]
        .into();
        for broken in [
            "LEGACY",
            "CATAPULTE_API_KEYS",
            "CATAPULTE_API_KEY_CI_SECRET",
            "CATAPULTE_API_KEY_CI_SENDER_DOMAINS",
            "CATAPULTE_API_KEY_CI_ADMIN",
        ] {
            let lookup = |k: &str| {
                if k == broken {
                    Err(bad())
                } else {
                    present
                        .get(k)
                        .map(|v| (*v).to_owned())
                        .ok_or(std::env::VarError::NotPresent)
                }
            };
            let err = AuthConfig::from_lookup("LEGACY", lookup).err().unwrap();
            assert!(err.to_string().contains(broken), "{broken}: {err}");
        }
    }

    #[test]
    fn a_named_key_without_a_secret_or_with_a_bad_flag_fails() {
        let lookup_for = |vars: Vec<(&'static str, &'static str)>| {
            move |k: &str| {
                vars.iter()
                    .find(|(name, _)| *name == k)
                    .map(|(_, v)| (*v).to_owned())
                    .ok_or(std::env::VarError::NotPresent)
            }
        };
        assert!(
            AuthConfig::from_lookup("LEGACY", lookup_for(vec![("CATAPULTE_API_KEYS", "ci")]))
                .is_err()
        );
        assert!(
            AuthConfig::from_lookup(
                "LEGACY",
                lookup_for(vec![
                    ("CATAPULTE_API_KEYS", "ci"),
                    ("CATAPULTE_API_KEY_CI_SECRET", "s"),
                    ("CATAPULTE_API_KEY_CI_ADMIN", "yes"),
                ])
            )
            .is_err()
        );
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
        let app = auth_test_router(legacy("secret"));
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
        let app = auth_test_router(legacy("secret"));
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
        let app = auth_test_router(legacy("secret"));
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
        let app = auth_test_router(legacy("secret"));
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
        let app = auth_test_router(legacy("secret"));
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
        let app = auth_test_router(legacy("secret"));
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
