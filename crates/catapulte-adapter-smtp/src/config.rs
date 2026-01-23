use std::time::Duration;

use lettre::Tokio1Executor;
use lettre::transport::smtp::authentication::Credentials;
use lettre::transport::smtp::client::{Tls, TlsParameters};
use lettre::transport::smtp::{AsyncSmtpTransport, AsyncSmtpTransportBuilder, PoolConfig};

/// SMTP connection configuration
#[derive(Clone, Debug, serde::Deserialize)]
pub struct SmtpConfig {
    #[serde(default = "SmtpConfig::default_hostname")]
    pub hostname: String,
    #[serde(default = "SmtpConfig::default_port")]
    pub port: u16,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default = "SmtpConfig::default_max_pool_size")]
    pub max_pool_size: u32,
    #[serde(default)]
    pub tls_enabled: bool,
    #[serde(default = "SmtpConfig::default_timeout")]
    pub timeout: u64,
    #[serde(default)]
    pub accept_invalid_cert: bool,
}

impl Default for SmtpConfig {
    fn default() -> Self {
        Self {
            hostname: Self::default_hostname(),
            port: Self::default_port(),
            username: None,
            password: None,
            max_pool_size: Self::default_max_pool_size(),
            tls_enabled: false,
            timeout: Self::default_timeout(),
            accept_invalid_cert: false,
        }
    }
}

impl SmtpConfig {
    fn default_hostname() -> String {
        "127.0.0.1".into()
    }

    fn default_port() -> u16 {
        25
    }

    fn default_max_pool_size() -> u32 {
        10
    }

    fn default_timeout() -> u64 {
        5000
    }

    fn get_credentials(&self) -> Option<Credentials> {
        if let Some(username) = self.username.as_ref() {
            tracing::debug!("using username {username:?}");
            let password = self.password.clone().unwrap_or_default();
            Some(Credentials::new(username.clone(), password))
        } else {
            tracing::debug!("using no username");
            None
        }
    }

    fn get_pool_config(&self) -> PoolConfig {
        tracing::debug!("with max pool size {}", self.max_pool_size);
        PoolConfig::default().max_size(self.max_pool_size)
    }

    fn get_timeout(&self) -> Duration {
        tracing::debug!("with timeout {}ms", self.timeout);
        Duration::from_millis(self.timeout)
    }

    fn get_tls(&self) -> Result<Tls, lettre::transport::smtp::Error> {
        if self.tls_enabled {
            tracing::debug!("with tls enabled");
            let parameters = TlsParameters::builder(self.hostname.to_string())
                .dangerous_accept_invalid_certs(self.accept_invalid_cert)
                .dangerous_accept_invalid_hostnames(true)
                .build_rustls()?;
            Ok(Tls::Required(parameters))
        } else {
            tracing::debug!("with tls disabled");
            Ok(Tls::None)
        }
    }

    fn get_transport(&self) -> Result<AsyncSmtpTransportBuilder, lettre::transport::smtp::Error> {
        tracing::debug!(
            "connecting to hostname {:?} on port {}",
            self.hostname,
            self.port
        );
        let result =
            AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(self.hostname.as_str())
                .port(self.port)
                .timeout(Some(self.get_timeout()))
                .pool_config(self.get_pool_config())
                .tls(self.get_tls()?);
        let result = if let Some(creds) = self.get_credentials() {
            result.credentials(creds)
        } else {
            result
        };
        Ok(result)
    }

    pub(crate) fn build(
        &self,
    ) -> Result<AsyncSmtpTransport<Tokio1Executor>, lettre::transport::smtp::Error> {
        tracing::debug!("building smtp pool");
        let mailer = self.get_transport()?;
        Ok(mailer.build())
    }
}
