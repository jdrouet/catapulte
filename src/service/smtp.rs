use crate::config::Config as RootConfig;
use crate::error::ServerError;
use lettre::transport::smtp::authentication::Credentials;
use lettre::transport::smtp::client::{Tls, TlsParameters};
use lettre::transport::smtp::{
    Error as LettreError, PoolConfig, SmtpTransport, SmtpTransportBuilder,
};
use std::string::ToString;
use std::sync::Arc;
use std::time::Duration;

pub type SmtpPool = SmtpTransport;

pub struct Config(pub Arc<RootConfig>);

impl Config {
    fn get_credentials(&self) -> Option<Credentials> {
        if let Some(username) = self.0.smtp_username.as_ref() {
            if let Some(password) = self.0.smtp_password.as_ref() {
                Some(Credentials::new(username.clone(), password.clone()))
            } else {
                Some(Credentials::new(username.clone(), String::new()))
            }
        } else {
            None
        }
    }

    fn get_pool_config(&self) -> PoolConfig {
        PoolConfig::default().max_size(self.0.smtp_max_pool_size)
    }

    fn get_timeout(&self) -> Duration {
        Duration::from_millis(self.0.smtp_timeout)
    }

    // TODO allow to add root certificate
    // TODO allow to accept invalid hostnames
    fn get_tls(&self) -> Result<Tls, SmtpError> {
        if self.0.smtp_tls_enabled {
            let parameteres = TlsParameters::builder(self.0.smtp_hostname.to_string())
                .dangerous_accept_invalid_certs(self.0.smtp_accept_invalid_cert)
                .build_rustls()?;
            Ok(Tls::Required(parameteres))
        } else {
            Ok(Tls::None)
        }
    }

    // TODO allow to specify authentication mechanism
    fn get_transport(&self) -> Result<SmtpTransportBuilder, SmtpError> {
        let result = SmtpTransport::builder_dangerous(self.0.smtp_hostname.as_str())
            .port(self.0.smtp_port)
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
    use crate::config::Config;

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
        let cfg = Config::from_args(vec![]);
        assert_eq!(cfg.smtp_hostname, "127.0.0.1");
        assert_eq!(cfg.smtp_port, 25);
        assert_eq!(cfg.smtp_username, None);
        assert_eq!(cfg.smtp_password, None);
        assert!(!cfg.smtp_tls_enabled);
        assert_eq!(cfg.smtp_max_pool_size, 10);
        assert_eq!(cfg.smtp_timeout, 5000);
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
        let _timeout = env_test_util::TempEnvVar::new("SMTP_TIMEOUT").with("3000");
        let cfg = Config::from_args(vec![]);
        assert_eq!(cfg.smtp_hostname, "mail.jolimail.io");
        assert_eq!(cfg.smtp_port, 1234);
        assert_eq!(cfg.smtp_username, Some("username".into()));
        assert_eq!(cfg.smtp_password, Some("password".into()));
        assert!(!cfg.smtp_tls_enabled);
        assert_eq!(cfg.smtp_max_pool_size, 2);
        assert_eq!(cfg.smtp_timeout, 3000);
        let cfg = super::Config(cfg.clone());
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
        let _timeout = env_test_util::TempEnvVar::new("SMTP_TIMEOUT").with("3000");
        let cfg = Config::from_args(vec![]);
        assert_eq!(cfg.smtp_hostname, "mail.jolimail.io");
        assert_eq!(cfg.smtp_port, 1234);
        assert_eq!(cfg.smtp_username, Some("username".into()));
        assert_eq!(cfg.smtp_password, Some("password".into()));
        assert!(cfg.smtp_tls_enabled);
        assert_eq!(cfg.smtp_max_pool_size, 2);
        assert_eq!(cfg.smtp_timeout, 3000);
        let cfg = super::Config(cfg.clone());
        assert!(cfg.get_credentials().is_some());
        let client = cfg.get_pool();
        assert!(client.is_ok());
    }
}
