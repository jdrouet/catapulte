use crate::error::ServerError;
use lettre::smtp::authentication::Credentials;
use lettre::smtp::error::Error as LettreError;
use lettre::smtp::r2d2::SmtpConnectionManager;
use lettre::{ClientSecurity, ClientTlsParameters, SmtpClient};
use native_tls::TlsConnector;
use r2d2::Pool;
use std::env;
use std::string::ToString;
use std::time::Duration;

pub type SmtpPool = Pool<SmtpConnectionManager>;

#[derive(Debug)]
pub struct Config {
    pub hostname: String,
    pub port: u16,
    pub username: Option<String>,
    pub password: Option<String>,
    pub max_pool_size: u32,
    pub tls_enabled: bool,
    pub timeout: u32,
}

impl Config {
    pub fn from_env() -> Self {
        Self {
            hostname: env::var("SMTP_HOSTNAME").unwrap_or_else(|_| String::from("localhost")),
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
            timeout: env::var("SMTP_TIMEOUT")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(5),
        }
    }

    fn get_security(&self) -> Result<ClientSecurity, SmtpError> {
        if self.tls_enabled {
            let tls_builder = TlsConnector::builder();
            // TODO customize TlsConnector min version
            let tls_builder = match tls_builder.build() {
                Ok(value) => value,
                Err(err) => return Err(SmtpError::TlsConnector(err.to_string())),
            };
            let tls_parameters = ClientTlsParameters::new(self.hostname.to_string(), tls_builder);
            Ok(ClientSecurity::Wrapper(tls_parameters))
        } else {
            Ok(ClientSecurity::None)
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

    fn get_client(&self) -> Result<SmtpClient, SmtpError> {
        let security = self.get_security()?;
        let client = match SmtpClient::new((self.hostname.as_str(), self.port), security) {
            Ok(client) => client,
            Err(err) => {
                return Err(SmtpError::PreconditionFailed(format!(
                    "couldn't create client: {}",
                    err
                )))
            }
        };
        // TODO customize timeout
        let client = client.timeout(Some(Duration::from_secs(5)));
        if let Some(creds) = self.get_credentials() {
            Ok(client.credentials(creds))
        } else {
            Ok(client)
        }
    }

    fn get_connection_manager(&self) -> Result<SmtpConnectionManager, SmtpError> {
        match SmtpConnectionManager::new(self.get_client()?) {
            Ok(manager) => Ok(manager),
            Err(_) => Err(SmtpError::PreconditionFailed(
                "couldn't create connection manager".into(),
            )),
        }
    }

    pub fn get_pool(&self) -> Result<SmtpPool, SmtpError> {
        let manager = self.get_connection_manager()?;
        match r2d2::Pool::builder()
            .max_size(self.max_pool_size)
            .build(manager)
        {
            Ok(pool) => Ok(pool),
            Err(_) => Err(SmtpError::PreconditionFailed("couldn't create pool".into())),
        }
    }
}

#[derive(Debug)]
pub enum SmtpError {
    PreconditionFailed(String),
    TlsConnector(String),
}

impl ToString for SmtpError {
    fn to_string(&self) -> String {
        match self {
            SmtpError::PreconditionFailed(msg) => {
                format!("Smtp Error: precondition failed ({})", msg)
            }
            SmtpError::TlsConnector(msg) => {
                format!("Smtp Error: unable to build tls connector ({})", msg)
            }
        }
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
        assert_eq!(cfg.hostname, "localhost");
        assert_eq!(cfg.port, 25);
        assert_eq!(cfg.username, None);
        assert_eq!(cfg.password, None);
        assert_eq!(cfg.tls_enabled, false);
        assert_eq!(cfg.max_pool_size, 10);
        assert_eq!(cfg.timeout, 5);
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
        assert_eq!(cfg.timeout, 3);
        assert!(cfg.get_credentials().is_some());
        let security = cfg.get_security();
        assert!(security.is_ok());
        assert!(matches!(security.unwrap(), ClientSecurity::None));
        let client = cfg.get_client();
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
        assert_eq!(cfg.timeout, 3);
        assert!(cfg.get_credentials().is_some());
        let security = cfg.get_security();
        assert!(security.is_ok());
        assert!(matches!(security.unwrap(), ClientSecurity::Wrapper(_)));
    }
}
