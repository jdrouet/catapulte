use crate::error::ServerError;
use lettre::transport::smtp::authentication::Credentials;
use lettre::transport::smtp::client::{Tls, TlsParameters};
use lettre::transport::smtp::{
    Error as LettreError, PoolConfig, SmtpTransport, SmtpTransportBuilder,
};
use std::env;
use std::string::ToString;
use std::time::Duration;

pub type SmtpPool = SmtpTransport;

fn env_var_u64(key: &str) -> Option<u64> {
    env::var(key).ok().and_then(|value| value.parse().ok())
}

#[derive(Debug)]
pub struct Config {
    pub hostname: String,
    pub port: u16,
    pub username: Option<String>,
    pub password: Option<String>,
    pub max_pool_size: u32,
    pub tls_enabled: bool,
    pub timeout: u64,
}

impl Config {
    pub fn from_env() -> Self {
        Self {
            hostname: env::var("SMTP_HOSTNAME").unwrap_or_else(|_| String::from("127.0.0.1")),
            port: env::var("SMTP_PORT")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(25),
            username: env::var("SMTP_USERNAME").ok(),
            password: env::var("SMTP_PASSWORD").ok(),
            max_pool_size: env::var("SMTP_MAX_POOL_SIZE")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(10),
            tls_enabled: env::var("SMTP_TLS_ENABLED")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(false),
            timeout: env_var_u64("SMTP_TIMEOUT")
                .map(|value| value * 1000)
                .or_else(|| env_var_u64("SMTP_TIMEOUT_MS"))
                .unwrap_or(5000),
        }
    }

    fn get_credentials(&self) -> Option<Credentials> {
        if let Some(username) = self.username.as_ref() {
            if let Some(password) = self.password.as_ref() {
                Some(Credentials::new(username.clone(), password.clone()))
            } else {
                Some(Credentials::new(username.clone(), String::new()))
            }
        } else {
            None
        }
    }

    fn get_pool_config(&self) -> PoolConfig {
        PoolConfig::default().max_size(self.max_pool_size)
    }

    fn get_timeout(&self) -> Option<Duration> {
        Some(Duration::from_millis(self.timeout))
    }

    // TODO allow to add root certificate
    // TODO allow to accept invalid hostnames
    // TODO allow to accept invalid certs
    fn get_tls(&self) -> Result<Tls, SmtpError> {
        let parameteres = TlsParameters::builder(self.hostname.to_string()).build()?;
        Ok(Tls::Required(parameteres))
    }

    // TODO allow to specify authentication mechanism
    fn get_transport(&self) -> Result<SmtpTransportBuilder, SmtpError> {
        let result = SmtpTransport::builder_dangerous(self.hostname.as_str())
            .port(self.port)
            .timeout(self.get_timeout())
            .pool_config(self.get_pool_config());
        let result = if self.tls_enabled {
            result.tls(self.get_tls()?)
        } else {
            result
        };
        let result = if let Some(creds) = self.get_credentials() {
            result.credentials(creds)
        } else {
            result
        };
        Ok(result)
    }

    pub fn get_pool(&self) -> Result<SmtpTransport, SmtpError> {
        let mailer = self.get_transport()?;
        Ok(mailer.build())
    }
}

#[derive(Debug)]
pub enum SmtpError {
    Configuration(String),
}

impl ToString for SmtpError {
    fn to_string(&self) -> String {
        match self {
            Self::Configuration(msg) => {
                format!("Smtp Error: configuration failed ({})", msg)
            }
        }
    }
}

impl From<LettreError> for SmtpError {
    fn from(err: LettreError) -> Self {
        Self::Configuration(err.to_string())
    }
}

impl From<LettreError> for ServerError {
    fn from(err: LettreError) -> Self {
        ServerError::InternalServerError(err.to_string())
    }
}

impl From<SmtpError> for ServerError {
    fn from(err: SmtpError) -> ServerError {
        ServerError::InternalServerError(err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[serial]
    fn config_default() {
        let _hostname = env_test_util::TempEnvVar::new("SMTP_HOSTNAME");
        let _port = env_test_util::TempEnvVar::new("SMTP_PORT");
        let _username = env_test_util::TempEnvVar::new("SMTP_USERNAME");
        let _password = env_test_util::TempEnvVar::new("SMTP_PASSWORD");
        let _tls_enabled = env_test_util::TempEnvVar::new("SMTP_TLS_ENABLED");
        let _max_pool_size = env_test_util::TempEnvVar::new("SMTP_MAX_POOL_SIZE");
        let _timeout = env_test_util::TempEnvVar::new("SMTP_TIMEOUT");
        let cfg = Config::from_env();
        assert_eq!(cfg.hostname, "127.0.0.1");
        assert_eq!(cfg.port, 25);
        assert_eq!(cfg.username, None);
        assert_eq!(cfg.password, None);
        assert_eq!(cfg.tls_enabled, false);
        assert_eq!(cfg.max_pool_size, 10);
        assert_eq!(cfg.timeout, 5000);
    }

    #[test]
    #[serial]
    fn config_simple() {
        let _hostname = env_test_util::TempEnvVar::new("SMTP_HOSTNAME").with("mail.jolimail.io");
        let _port = env_test_util::TempEnvVar::new("SMTP_PORT").with("1234");
        let _username = env_test_util::TempEnvVar::new("SMTP_USERNAME").with("username");
        let _password = env_test_util::TempEnvVar::new("SMTP_PASSWORD").with("password");
        let _tls_enabled = env_test_util::TempEnvVar::new("SMTP_TLS_ENABLED").with("false");
        let _max_pool_size = env_test_util::TempEnvVar::new("SMTP_MAX_POOL_SIZE").with("2");
        let _timeout = env_test_util::TempEnvVar::new("SMTP_TIMEOUT").with("3");
        let cfg = Config::from_env();
        assert_eq!(cfg.hostname, "mail.jolimail.io");
        assert_eq!(cfg.port, 1234);
        assert_eq!(cfg.username, Some("username".into()));
        assert_eq!(cfg.password, Some("password".into()));
        assert_eq!(cfg.tls_enabled, false);
        assert_eq!(cfg.max_pool_size, 2);
        assert_eq!(cfg.timeout, 3000);
        assert!(cfg.get_credentials().is_some());
        let client = cfg.get_transport();
        assert!(client.is_ok());
    }

    #[test]
    #[serial]
    fn config_tls() {
        let _hostname = env_test_util::TempEnvVar::new("SMTP_HOSTNAME").with("mail.jolimail.io");
        let _port = env_test_util::TempEnvVar::new("SMTP_PORT").with("1234");
        let _username = env_test_util::TempEnvVar::new("SMTP_USERNAME").with("username");
        let _password = env_test_util::TempEnvVar::new("SMTP_PASSWORD").with("password");
        let _tls_enabled = env_test_util::TempEnvVar::new("SMTP_TLS_ENABLED").with("true");
        let _max_pool_size = env_test_util::TempEnvVar::new("SMTP_MAX_POOL_SIZE").with("2");
        let _timeout = env_test_util::TempEnvVar::new("SMTP_TIMEOUT").with("3");
        let cfg = Config::from_env();
        assert_eq!(cfg.hostname, "mail.jolimail.io");
        assert_eq!(cfg.port, 1234);
        assert_eq!(cfg.username, Some("username".into()));
        assert_eq!(cfg.password, Some("password".into()));
        assert_eq!(cfg.tls_enabled, true);
        assert_eq!(cfg.max_pool_size, 2);
        assert_eq!(cfg.timeout, 3000);
        assert!(cfg.get_credentials().is_some());
        let client = cfg.get_pool();
        assert!(client.is_ok());
    }
}
