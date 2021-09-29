use crate::config::{env_bool, env_number, env_str, parse_number};
use crate::error::ServerError;
use clap::{App, Arg, ArgMatches};
use lettre::transport::smtp::authentication::Credentials;
use lettre::transport::smtp::client::{Tls, TlsParameters};
use lettre::transport::smtp::{
    Error as LettreError, PoolConfig, SmtpTransport, SmtpTransportBuilder,
};
use std::string::ToString;
use std::time::Duration;

pub type SmtpPool = SmtpTransport;

#[derive(Debug)]
pub struct Config {
    pub hostname: String,
    pub port: u16,
    pub username: Option<String>,
    pub password: Option<String>,
    pub max_pool_size: u32,
    pub tls_enabled: bool,
    pub timeout: u64,
    pub accept_invalid_cert: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            hostname: "127.0.0.1".into(),
            port: 25,
            username: None,
            password: None,
            max_pool_size: 10,
            tls_enabled: false,
            timeout: 5000,
            accept_invalid_cert: false,
        }
    }
}

impl Config {
    pub fn with_args(app: App) -> App {
        app.arg(
            Arg::new("smtp_hostname")
                .long("smtp-hostname")
                .about("Hostname of the SMTP server"),
        )
        .arg(
            Arg::new("smtp_port")
                .long("smtp-port")
                .about("Port of the SMTP server"),
        )
        .arg(
            Arg::new("smtp_username")
                .long("smtp-username")
                .about("Username to authenticated with the SMTP server"),
        )
        .arg(
            Arg::new("smtp_password")
                .long("smtp-password")
                .about("Password to authenticated with the SMTP server"),
        )
        .arg(
            Arg::new("smtp_max_pool_size")
                .long("smtp-max-pool-size")
                .about("Max pool size for the SMTP connections"),
        )
        .arg(
            Arg::new("smtp_tls_enabled")
                .long("smtp-tls-enabled")
                .about("Enable TLS with the SMTP server"),
        )
        .arg(
            Arg::new("smtp_timeout")
                .long("smtp-timeout")
                .about("Enable TLS with the SMTP server"),
        )
        .arg(
            Arg::new("smtp_accept_invalid_cert")
                .long("smtp-accept-invalid-cert")
                .about("Accept invalid certificates for TLS connection with the SMTP server"),
        )
    }
}

impl From<&ArgMatches> for Config {
    fn from(matches: &ArgMatches) -> Self {
        let default = Self::default();
        let hostname = matches
            .value_of("smtp_hostname")
            .map(String::from)
            .or_else(|| env_str("SMTP_HOSTNAME"))
            .unwrap_or(default.hostname);
        let port = matches
            .value_of("smtp_port")
            .map(|value| parse_number("smtp-port", value))
            .or_else(|| env_number("SMTP_PORT"))
            .unwrap_or(default.port);
        let username = matches
            .value_of("smtp_username")
            .map(String::from)
            .or_else(|| env_str("SMTP_USERNAME"));
        let password = matches
            .value_of("smtp_password")
            .map(String::from)
            .or_else(|| env_str("SMTP_PASSWORD"));
        let max_pool_size = matches
            .value_of("smtp_max_pool_size")
            .map(|value| parse_number("smtp-max-pool-size", value))
            .or_else(|| env_number("SMTP_MAX_POOL_SIZE"))
            .unwrap_or(default.max_pool_size);
        let tls_enabled =
            matches.is_present("smtp_tls_enabled") || env_bool("SMTP_TLS_ENABLED").unwrap_or(false);
        let timeout = matches
            .value_of("smtp_timeout")
            .map(|value| parse_number("smtp-timeout", value))
            .or_else(|| env_number("SMTP_TIMEOUT"))
            .unwrap_or(default.timeout);
        let accept_invalid_cert = matches.is_present("smtp_accept_invalid_cert")
            || env_bool("SMTP_ACCEPT_INVALID_CERT").unwrap_or(false);

        Self {
            hostname,
            port,
            username,
            password,
            max_pool_size,
            tls_enabled,
            timeout,
            accept_invalid_cert,
        }
    }
}

impl Config {
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

    fn get_timeout(&self) -> Duration {
        Duration::from_millis(self.timeout)
    }

    // TODO allow to add root certificate
    // TODO allow to accept invalid hostnames
    fn get_tls(&self) -> Result<Tls, SmtpError> {
        if self.tls_enabled {
            let parameteres = TlsParameters::builder(self.hostname.to_string())
                .dangerous_accept_invalid_certs(self.accept_invalid_cert)
                .build_rustls()?;
            Ok(Tls::Required(parameteres))
        } else {
            Ok(Tls::None)
        }
    }

    // TODO allow to specify authentication mechanism
    fn get_transport(&self) -> Result<SmtpTransportBuilder, SmtpError> {
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
        let cfg = Config::from_args(vec![]).smtp;
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
        let _timeout = env_test_util::TempEnvVar::new("SMTP_TIMEOUT").with("3000");
        let cfg = Config::from_args(vec![]).smtp;
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
        let _timeout = env_test_util::TempEnvVar::new("SMTP_TIMEOUT").with("3000");
        let cfg = Config::from_args(vec![]).smtp;
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
