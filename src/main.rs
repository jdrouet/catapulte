#[cfg(test)]
#[macro_use]
extern crate serial_test;

#[macro_use]
extern crate lazy_static;
#[macro_use]
extern crate log;

use actix_web::middleware::{Compress, DefaultHeaders, Logger};
use actix_web::{web, App, HttpServer};

mod controller;
mod error;
mod middleware;
mod service;

struct Config {
    pub address: String,
    pub port: String,
}

impl Config {
    pub fn from_env() -> Self {
        Self {
            address: std::env::var("ADDRESS").unwrap_or_else(|_| String::from("127.0.0.1")),
            port: std::env::var("PORT").unwrap_or_else(|_| String::from("3000")),
        }
    }

    fn to_bind(&self) -> String {
        format!("{}:{}", self.address, self.port)
    }
}

macro_rules! create_app {
    () => {
        App::new().app_data(web::JsonConfig::default().error_handler(error::json_error_handler))
    };
}

macro_rules! bind_services {
    ($app: expr) => {
        $app.service(controller::status::handler)
            .configure(controller::templates::config)
            .configure(controller::swagger::config)
    };
}

#[cfg(not(tarpaulin_include))]
#[actix_rt::main]
async fn main() -> std::io::Result<()> {
    env_logger::init();

    let server_cfg = Config::from_env();
    let smtp_cfg = service::smtp::Config::from_env();

    let template_provider =
        service::template::provider::TemplateProvider::from_env().expect("template provider init");
    let smtp_pool = smtp_cfg.get_pool().expect("smtp service init");

    info!("starting server");
    HttpServer::new(move || {
        bind_services!(create_app!()
            .data(template_provider.clone())
            .data(smtp_pool.clone())
            .wrap(DefaultHeaders::new().header("X-Version", env!("CARGO_PKG_VERSION")))
            .wrap(Logger::default())
            .wrap(Compress::default()))
    })
    .bind(server_cfg.to_bind())?
    .run()
    .await
}

#[cfg(test)]
#[cfg(not(tarpaulin_include))]
mod tests {
    use super::service::smtp::Config;
    use super::service::template::provider::TemplateProvider;
    use super::*;
    use actix_http::Request;
    use actix_web::dev::ServiceResponse;
    use actix_web::{test, App};
    use env_test_util::TempEnvVar;
    use reqwest;
    use serde::Deserialize;
    use uuid::Uuid;

    pub fn env_str(key: &str, default_value: &str) -> String {
        std::env::var(key)
            .ok()
            .unwrap_or_else(|| default_value.into())
    }

    lazy_static! {
        pub static ref INBOX_HOSTNAME: String = env_str("TEST_INBOX_HOSTNAME", "localhost");
        pub static ref INBOX_PORT: String = env_str("TEST_INBOX_PORT", "1080");
        pub static ref SMTP_HOSTNAME: String = env_str("TEST_SMTP_HOSTNAME", "localhost");
        pub static ref SMTP_PORT: String = env_str("TEST_SMTP_PORT", "1025");
        pub static ref SMTPS_HOSTNAME: String = env_str("TEST_SMTPS_HOSTNAME", "localhost");
        pub static ref SMTPS_PORT: String = env_str("TEST_SMTPS_PORT", "1025");
    }

    #[derive(Debug, Default)]
    pub struct ServerBuilder {
        authenticated: bool,
        invalid_cert: bool,
        secure: bool,
    }

    impl ServerBuilder {
        pub fn authenticated(mut self, value: bool) -> Self {
            self.authenticated = value;
            self
        }

        pub fn invalid_cert(mut self, value: bool) -> Self {
            self.invalid_cert = value;
            self
        }

        pub fn secure(mut self, value: bool) -> Self {
            self.secure = value;
            self
        }

        fn set_variables(&self) -> Vec<TempEnvVar> {
            let mut res = Vec::new();
            if self.authenticated {
                res.push(TempEnvVar::new("AUTHENTICATION_ENABLED").with("true"));
            }
            if self.secure {
                res.push(TempEnvVar::new("SMTP_HOSTNAME").with(SMTPS_HOSTNAME.as_str()));
                res.push(TempEnvVar::new("SMTP_PORT").with(SMTPS_PORT.as_str()));
                res.push(TempEnvVar::new("SMTP_TLS_ENABLED").with("true"));
                if self.invalid_cert {
                    res.push(TempEnvVar::new("SMTP_ACCEPT_INVALID_CERT").with("true"));
                }
            } else {
                res.push(TempEnvVar::new("SMTP_HOSTNAME").with(SMTP_HOSTNAME.as_str()));
                res.push(TempEnvVar::new("SMTP_PORT").with(SMTP_PORT.as_str()));
            }
            res
        }

        pub async fn execute(&self, req: Request) -> ServiceResponse {
            let _variables = self.set_variables();
            let template_provider = TemplateProvider::from_env().expect("template provider init");
            let smtp_pool = Config::from_env().get_pool().expect("smtp service init");
            let mut app = test::init_service(bind_services!(create_app!()
                .data(template_provider.clone())
                .data(smtp_pool.clone())))
            .await;
            test::call_service(&mut app, req).await
        }
    }

    #[derive(Deserialize)]
    pub struct Email {
        pub html: String,
        pub text: String,
    }

    pub async fn get_latest_inbox(from: &String, to: &String) -> Vec<Email> {
        let url = format!(
            "http://{}:{}/api/emails?from={}&to={}",
            INBOX_HOSTNAME.as_str(),
            INBOX_PORT.as_str(),
            from,
            to
        );
        reqwest::get(url.as_str())
            .await
            .unwrap()
            .json::<Vec<Email>>()
            .await
            .unwrap()
    }

    pub fn create_email() -> String {
        format!("{}@example.com", Uuid::new_v4())
    }

    #[test]
    #[serial]
    fn test_get_address() {
        let _address = env_test_util::TempEnvVar::new("ADDRESS");
        assert_eq!(super::Config::from_env().address, "127.0.0.1");
        let _address = _address.with("something");
        assert_eq!(super::Config::from_env().address, "something");
    }

    #[test]
    #[serial]
    fn test_get_port() {
        let _port = env_test_util::TempEnvVar::new("PORT");
        assert_eq!(super::Config::from_env().port, "3000");
        let _port = _port.with("1234");
        assert_eq!(super::Config::from_env().port, "1234");
    }

    #[test]
    #[serial]
    fn test_bind() {
        let _address = env_test_util::TempEnvVar::new("ADDRESS");
        let _port = env_test_util::TempEnvVar::new("PORT");
        assert_eq!(super::Config::from_env().to_bind(), "127.0.0.1:3000");
        let _address = _address.with("something");
        let _port = _port.with("1234");
        assert_eq!(super::Config::from_env().to_bind(), "something:1234");
    }
}
