#[cfg(test)]
#[macro_use]
extern crate serial_test;

#[macro_use]
extern crate lazy_static;
#[macro_use]
extern crate log;

use actix_web::middleware::{Compress, DefaultHeaders, Logger};
use actix_web::{App, HttpServer};

mod config;
mod controller;
mod error;
mod middleware;
mod service;

macro_rules! create_app {
    () => {
        App::new().app_data(
            actix_web::web::JsonConfig::default().error_handler(crate::error::json_error_handler),
        )
    };
}

macro_rules! bind_services {
    ($app: expr) => {
        $app.service(crate::controller::status::handler)
            .configure(crate::controller::templates::config)
            .configure(crate::controller::swagger::config)
    };
}

#[cfg(not(tarpaulin_include))]
#[actix_rt::main]
async fn main() -> std::io::Result<()> {
    env_logger::init();

    let cfg = config::Config::parse();

    let template_provider =
        service::template::provider::TemplateProvider::from_env().expect("template provider init");
    let smtp_pool = cfg.smtp.get_pool().expect("smtp service init");

    info!("starting server");
    HttpServer::new(move || {
        bind_services!(create_app!()
            .data(template_provider.clone())
            .data(smtp_pool.clone())
            .wrap(DefaultHeaders::new().header("X-Version", env!("CARGO_PKG_VERSION")))
            .wrap(Logger::default())
            .wrap(Compress::default()))
    })
    .bind(cfg.server.to_bind())?
    .run()
    .await
}

#[cfg(test)]
#[cfg(not(tarpaulin_include))]
mod tests {
    use super::service::template::provider::TemplateProvider;
    use crate::config::{env_number, env_str, Config};
    use actix_http::Request;
    use actix_web::dev::ServiceResponse;
    use actix_web::{test, App};
    use reqwest;
    use serde::Deserialize;
    use uuid::Uuid;

    lazy_static! {
        pub static ref INBOX_HOSTNAME: String =
            env_str("TEST_INBOX_HOSTNAME").unwrap_or_else(|| "localhost".to_string());
        pub static ref INBOX_PORT: u16 = env_number("TEST_INBOX_PORT").unwrap_or(1080);
        pub static ref SMTP_HOSTNAME: String =
            env_str("TEST_SMTP_HOSTNAME").unwrap_or_else(|| "localhost".to_string());
        pub static ref SMTP_PORT: u16 = env_number("TEST_SMTP_PORT").unwrap_or(1025);
        pub static ref SMTPS_HOSTNAME: String =
            env_str("TEST_SMTPS_HOSTNAME").unwrap_or_else(|| "localhost".to_string());
        pub static ref SMTPS_PORT: u16 = env_number("TEST_SMTPS_PORT").unwrap_or(1025);
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

        fn build_config(&self) -> Config {
            let mut cfg = Config::default();
            if self.authenticated {
                // res.push(TempEnvVar::new("AUTHENTICATION_ENABLED").with("true"));
            }
            if self.secure {
                cfg.smtp.hostname = SMTPS_HOSTNAME.to_string();
                cfg.smtp.port = *SMTPS_PORT;
                cfg.smtp.tls_enabled = true;
                if self.invalid_cert {
                    cfg.smtp.accept_invalid_cert = true;
                }
            } else {
                cfg.smtp.hostname = SMTP_HOSTNAME.to_string();
                cfg.smtp.port = *SMTP_PORT;
            }
            cfg
        }

        pub async fn execute(&self, req: Request) -> ServiceResponse {
            let cfg = self.build_config();
            let template_provider = TemplateProvider::from_env().expect("template provider init");
            let smtp_pool = cfg.smtp.get_pool().expect("smtp service init");
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
            *INBOX_PORT,
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
}
