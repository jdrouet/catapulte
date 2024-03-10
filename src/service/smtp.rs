use crate::error::ServerError;
use lettre::transport::smtp::authentication::Credentials;
use lettre::transport::smtp::client::{Tls, TlsParameters};
use lettre::transport::smtp::{
    Error as LettreError, PoolConfig, SmtpTransport, SmtpTransportBuilder,
};
use std::time::Duration;

pub type SmtpPool = SmtpTransport;

#[derive(Clone, Debug, serde::Deserialize)]
pub(crate) struct Configuration {
    #[serde(default = "Configuration::default_hostname")]
    pub hostname: String,
    #[serde(default = "Configuration::default_port")]
    pub port: u16,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default = "Configuration::default_max_pool_size")]
    pub max_pool_size: u32,
    #[serde(default)]
    pub tls_enabled: bool,
    #[serde(default = "Configuration::default_timeout")]
    pub timeout: u64,
    #[serde(default)]
    pub accept_invalid_cert: bool,
}

impl Default for Configuration {
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

impl Configuration {
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
}

#[cfg(test)]
impl Configuration {
    pub(crate) fn insecure() -> Self {
        Self {
            hostname: crate::tests::env_str("TEST_SMTP_HOSTNAME")
                .unwrap_or_else(|| "localhost".to_string()),
            port: crate::tests::env_number("TEST_SMTP_PORT").unwrap_or(1025),
            username: None,
            password: None,
            max_pool_size: Self::default_max_pool_size(),
            tls_enabled: false,
            timeout: Self::default_timeout(),
            accept_invalid_cert: false,
        }
    }
    pub(crate) fn secure() -> Self {
        Self {
            hostname: crate::tests::env_str("TEST_SMTPS_HOSTNAME")
                .unwrap_or_else(|| "localhost".to_string()),
            port: crate::tests::env_number("TEST_SMTPS_PORT").unwrap_or(1026),
            username: None,
            password: None,
            max_pool_size: Self::default_max_pool_size(),
            tls_enabled: true,
            timeout: Self::default_timeout(),
            accept_invalid_cert: true,
        }
    }
}

impl Configuration {
    fn get_credentials(&self) -> Option<Credentials> {
        if let Some(username) = self.username.as_ref() {
            tracing::debug!("using username {username:?}");
            if let Some(password) = self.password.as_ref() {
                Some(Credentials::new(username.clone(), password.clone()))
            } else {
                Some(Credentials::new(username.clone(), String::new()))
            }
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

    // TODO allow to add root certificate
    // TODO allow to accept invalid hostnames
    fn get_tls(&self) -> Result<Tls, ConfigurationError> {
        if self.tls_enabled {
            tracing::debug!("with tls enabled");
            let parameteres = TlsParameters::builder(self.hostname.to_string())
                .dangerous_accept_invalid_certs(self.accept_invalid_cert)
                .build_rustls()?;
            Ok(Tls::Required(parameteres))
        } else {
            tracing::debug!("with tls disabled");
            Ok(Tls::None)
        }
    }

    // TODO allow to specify authentication mechanism
    fn get_transport(&self) -> Result<SmtpTransportBuilder, ConfigurationError> {
        tracing::debug!(
            "connecting to hostname {:?} on port {}",
            self.hostname,
            self.port
        );
        let result = SmtpTransport::builder_dangerous(self.hostname.as_str())
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

    pub(crate) fn build(&self) -> Result<SmtpTransport, ConfigurationError> {
        tracing::debug!("building smtp pool");
        let mailer = self.get_transport()?;
        Ok(mailer.build())
    }
}

#[derive(Debug)]
pub(crate) struct ConfigurationError(#[allow(dead_code)] LettreError);

impl From<LettreError> for ConfigurationError {
    fn from(err: LettreError) -> Self {
        tracing::error!("smtp configuration error: {:?}", err);
        Self(err)
    }
}

impl From<LettreError> for ServerError {
    fn from(err: LettreError) -> Self {
        if let Some(code) = err.status() {
            metrics::counter!(
                "smtp_error",
                "severity" => code.severity.to_string(),
                "category" => code.category.to_string(),
                "detail" => code.detail.to_string(),
            )
            .increment(1);
        } else {
            metrics::counter!("smtp_error").increment(1);
        }
        tracing::error!("smtp error: {:?}", err);
        ServerError::internal().details(serde_json::json!({
            "origin": "smtp"
        }))
    }
}
