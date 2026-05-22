use std::sync::Arc;

use anyhow::Context;
use catapulte_domain::use_case::submit_email::{SubmitEmailService, SubmitEmailUseCase};
use catapulte_inbound_http::{HttpServerState, InboundHttpConfig, InboundHttpServer};
use catapulte_outbound_sqlite::{SqliteAdapter, SqliteConfig};

#[derive(Clone)]
struct AppState {
    submit_email: Arc<SubmitEmailService<SqliteAdapter, SqliteAdapter>>,
}

impl HttpServerState for AppState {
    fn submit_email(&self) -> &impl SubmitEmailUseCase {
        self.submit_email.as_ref()
    }
}

pub struct AppConfig {
    pub sqlite: SqliteConfig,
    pub http: InboundHttpConfig,
}

impl AppConfig {
    /// # Errors
    ///
    /// Returns an error when any sub-config cannot be loaded from environment variables.
    pub fn from_env() -> anyhow::Result<Self> {
        let sqlite = SqliteConfig::from_env("CATAPULTE_SQLITE").context("loading sqlite config")?;
        let http = InboundHttpConfig::from_env("CATAPULTE_HTTP").context("loading http config")?;
        Ok(Self { sqlite, http })
    }

    /// # Errors
    ///
    /// Returns an error when the sqlite adapter fails to build or migrations fail to run.
    pub async fn build(self) -> anyhow::Result<Application> {
        let sqlite = self
            .sqlite
            .build()
            .await
            .context("building sqlite adapter")?;
        sqlite
            .migrate()
            .await
            .context("running sqlite migrations")?;

        let submit_email = Arc::new(SubmitEmailService::new(sqlite.clone(), sqlite));
        let state = AppState { submit_email };
        let server = self.http.build();

        Ok(Application { state, server })
    }
}

pub struct Application {
    state: AppState,
    server: InboundHttpServer,
}

impl Application {
    /// # Errors
    ///
    /// Returns an error when the HTTP server fails to bind or exits unexpectedly.
    pub async fn run(self) -> anyhow::Result<()> {
        tracing::info!("catapulte starting");
        self.server
            .run(self.state)
            .await
            .context("running http server")
    }
}
