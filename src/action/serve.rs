use catapulte_adapter_http::{HttpConfig, HttpServer};
use catapulte_adapter_smtp::{SmtpConfig, SmtpSender};
use catapulte_adapter_template::{
    HttpLoader, HttpLoaderConfig, LocalLoader, LocalLoaderConfig, MrmlRenderer, MrmlRendererConfig,
    MultiLoader,
};
use catapulte_domain::service::SendEmailService;

/// Application configuration combining all adapter configs
#[derive(Clone, Debug, serde::Deserialize)]
pub struct Configuration {
    #[serde(default)]
    pub http: HttpConfig,
    #[serde(default)]
    pub smtp: SmtpConfig,
    #[serde(default)]
    pub template: TemplateConfig,
    #[serde(default)]
    pub renderer: MrmlRendererConfig,
}

/// Template loading configuration
#[derive(Clone, Debug, Default, serde::Deserialize)]
pub struct TemplateConfig {
    #[serde(default)]
    pub local: LocalLoaderConfig,
    #[serde(default)]
    pub http: Option<HttpLoaderConfig>,
}

impl Configuration {
    pub fn from_path(path: &str) -> Self {
        config::Config::builder()
            .add_source(config::File::with_name(path).required(false))
            .add_source(config::Environment::default().separator("__"))
            .build()
            .unwrap()
            .try_deserialize()
            .unwrap()
    }
}

#[derive(clap::Parser)]
pub(crate) struct Action {
    /// Path to the configuration toml file, default to /etc/catapulte/catapulte.toml.
    #[clap(
        short,
        long,
        default_value = "/etc/catapulte/catapulte.toml",
        env = "CATAPULTE_CONFIG"
    )]
    pub config_path: String,
}

impl Action {
    pub(crate) async fn execute(self) {
        let config = Configuration::from_path(&self.config_path);

        // Build template loader (local + optional HTTP)
        let mut loader = MultiLoader::new().with_local(LocalLoader::new(&config.template.local));
        if let Some(http_config) = &config.template.http {
            loader = loader.with_http(HttpLoader::new(http_config));
        }

        // Build renderer
        let renderer = MrmlRenderer::new(&config.renderer);

        // Build SMTP sender
        let sender = SmtpSender::new(&config.smtp).expect("failed to build SMTP sender");

        // Create the email service
        let service = SendEmailService::new(loader, renderer, sender);

        // Create and run the HTTP server
        let server = HttpServer::new(config.http, service);
        server.run().await;
    }
}
