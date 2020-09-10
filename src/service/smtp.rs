use crate::error::ServerError;
use lettre::smtp::authentication::Credentials;
use lettre::smtp::error::Error as LettreError;
use lettre::smtp::r2d2::SmtpConnectionManager;
use lettre::{ClientSecurity, SmtpClient};
use r2d2::Pool;
use std::env;
use std::string::ToString;
use std::time::Duration;

#[derive(Debug)]
pub enum SmtpError {
    PreconditionFailed(String),
}

impl ToString for SmtpError {
    fn to_string(&self) -> String {
        match self {
            SmtpError::PreconditionFailed(msg) => {
                format!("Smtp Error: precondition failed ({})", msg)
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

fn get_smtp_hostname() -> String {
    env::var("SMTP_HOSTNAME").unwrap_or("localhost".into())
}

fn get_smtp_port() -> u16 {
    env::var("SMTP_PORT")
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(25u16)
}

fn get_smtp_username() -> Option<String> {
    env::var("SMTP_USERNAME").ok()
}

fn get_smtp_password() -> Option<String> {
    env::var("SMTP_PASSWORD").ok()
}

fn get_credentials() -> Option<Credentials> {
    let username = get_smtp_username();
    let password = get_smtp_password();
    if username.is_none() && password.is_none() {
        None
    } else {
        let username = username.unwrap_or("".into());
        let password = password.unwrap_or("".into());
        Some(Credentials::new(username.into(), password.into()))
    }
}

fn get_security() -> ClientSecurity {
    // TODO
    ClientSecurity::None
}

fn get_client() -> Result<SmtpClient, SmtpError> {
    let domain = get_smtp_hostname();
    let port = get_smtp_port();
    let security = get_security();
    let client = match SmtpClient::new((domain.as_str(), port), security) {
        Ok(client) => client,
        Err(_) => {
            return Err(SmtpError::PreconditionFailed(
                "couldn't create client".into(),
            ))
        }
    };
    let client = client.timeout(Some(Duration::from_secs(5)));
    if let Some(creds) = get_credentials() {
        Ok(client.credentials(creds))
    } else {
        Ok(client)
    }
}

fn get_connection_manager() -> Result<SmtpConnectionManager, SmtpError> {
    match SmtpConnectionManager::new(get_client()?) {
        Ok(manager) => Ok(manager),
        Err(_) => Err(SmtpError::PreconditionFailed(
            "couldn't create connection manager".into(),
        )),
    }
}

pub type SmtpPool = Pool<SmtpConnectionManager>;

pub fn get_pool() -> Result<SmtpPool, SmtpError> {
    let manager = get_connection_manager()?;
    match r2d2::Pool::builder().max_size(15).build(manager) {
        Ok(pool) => Ok(pool),
        Err(_) => Err(SmtpError::PreconditionFailed("couldn't create pool".into())),
    }
}
