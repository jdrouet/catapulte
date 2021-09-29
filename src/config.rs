use crate::service::smtp::Config as SmtpConfig;
use clap::{crate_description, crate_name, crate_version, App, Arg, ArgMatches, Clap};
use std::env;
use std::str::FromStr;

pub(crate) fn env_bool<T: FromStr>(key: &str) -> Option<T> {
    env_str(key).map(|value| {
        value
            .parse::<T>()
            .map_err(|_err| format!("{} should be a boolean (got {})", key, value))
            .unwrap()
    })
}

pub(crate) fn parse_number<T: FromStr>(key: &str, value: &str) -> T {
    value
        .parse::<T>()
        .map_err(|_err| format!("{} should be a number (got {})", key, value))
        .unwrap()
}

pub(crate) fn env_number<T: FromStr>(key: &str) -> Option<T> {
    env_str(key).map(|value| parse_number(key, &value))
}

pub(crate) fn env_str(key: &str) -> Option<String> {
    env::var(key).ok()
}

#[derive(Clap)]
pub struct ServerConfig {
    address: String,
    port: u16,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            address: "localhost".into(),
            port: 3000,
        }
    }
}

impl From<&ArgMatches> for ServerConfig {
    fn from(matches: &ArgMatches) -> Self {
        let default = Self::default();

        Self {
            address: matches
                .value_of("server_address")
                .map(String::from)
                .or_else(|| env_str("ADDRESS"))
                .unwrap_or(default.address),
            port: matches
                .value_of("server_port")
                .map(|value| parse_number("port", value))
                .or_else(|| env_number("PORT"))
                .unwrap_or(default.port),
        }
    }
}

impl ServerConfig {
    pub fn to_bind(&self) -> String {
        format!("{}:{}", self.address, self.port)
    }

    pub fn with_args(app: App) -> App {
        app.arg(
            Arg::new("server_address")
                .long("address")
                .about("Address to bind the server"),
        )
        .arg(
            Arg::new("server_port")
                .long("port")
                .about("Port to bind the server"),
        )
    }
}

pub struct Config {
    pub server: ServerConfig,
    pub smtp: SmtpConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            server: ServerConfig::default(),
            smtp: SmtpConfig::default(),
        }
    }
}

impl From<ArgMatches> for Config {
    fn from(matches: ArgMatches) -> Self {
        Self {
            server: ServerConfig::from(&matches),
            smtp: SmtpConfig::from(&matches),
        }
    }
}

impl Config {
    pub fn build_app<'a>() -> App<'a> {
        let app = App::new(crate_name!())
            .version(crate_version!())
            .about(crate_description!());
        let app = ServerConfig::with_args(app);
        SmtpConfig::with_args(app)
    }

    pub fn parse() -> Self {
        let app = Self::build_app();
        let matches = app.get_matches();
        Self::from(matches)
    }

    #[cfg(test)]
    pub(crate) fn from_args(values: Vec<&str>) -> Self {
        Self::from(Self::build_app().get_matches_from(values))
    }
}
