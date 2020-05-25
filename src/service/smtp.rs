use crate::error::ServerError;
use lettre::smtp::authentication::Credentials;
use lettre::smtp::r2d2::SmtpConnectionManager;
use lettre::{ClientSecurity, SmtpClient};
use r2d2::Pool;
use std::env;
use std::time::Duration;
use url::Url;

#[derive(Debug)]
pub enum SmtpError {
    PreconditionFailed(String),
}

impl From<SmtpError> for ServerError {
    fn from(err: SmtpError) -> ServerError {
        ServerError::Smtp(err)
    }
}

fn get_smtp_url() -> Result<Url, SmtpError> {
    let url = match env::var("SMTP_URL") {
        Ok(value) => value,
        Err(_) => "smtp://127.0.0.1:1025".into(),
    };
    match Url::parse(url.as_str()) {
        Ok(value) => Ok(value),
        Err(_) => Err(SmtpError::PreconditionFailed("couldn't parse url".into())),
    }
}

fn get_smtp_domain(url: &Url) -> String {
    match url.host().and_then(|host| Some(host.to_string())) {
        Some(value) => value,
        None => "127.0.0.1".into(),
    }
}

fn get_smtp_port(url: &Url) -> u16 {
    match url.port() {
        Some(value) => value,
        None => 25u16,
    }
}

fn get_credentials(url: &Url) -> Option<Credentials> {
    let username = url.username();
    let password = url.password();
    if username.len() == 0 && password.is_none() {
        None
    } else {
        let password = password.or_else(|| Some("")).unwrap();
        Some(Credentials::new(username.into(), password.into()))
    }
}

fn get_security(_url: &Url) -> ClientSecurity {
    // TODO
    ClientSecurity::None
}

fn get_client() -> Result<SmtpClient, SmtpError> {
    let url = get_smtp_url()?;
    let domain = get_smtp_domain(&url);
    let port = get_smtp_port(&url);
    let security = get_security(&url);
    let client = match SmtpClient::new((domain.as_str(), port), security) {
        Ok(client) => client,
        Err(_) => {
            return Err(SmtpError::PreconditionFailed(
                "couldn't create client".into(),
            ))
        }
    };
    let client = client.timeout(Some(Duration::from_secs(5)));
    let client = match get_credentials(&url) {
        Some(creds) => client.credentials(creds),
        None => client,
    };
    Ok(client)
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
