#[cfg(test)]
#[macro_use]
extern crate serial_test;

#[macro_use]
extern crate lazy_static;
#[macro_use]
extern crate log;

use actix_web::middleware::{Compress, DefaultHeaders, Logger};
use actix_web::{web::Data, App, HttpServer};

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
    ($cfg: expr, $app: expr) => {
        $app.service(crate::controller::status::handler)
            .configure(|app| crate::controller::templates::config($cfg, app))
            .configure(|app| crate::controller::swagger::config($cfg, app))
    };
}

#[cfg(not(tarpaulin_include))]
#[actix_rt::main]
async fn main() -> std::io::Result<()> {
    env_logger::init();

    let cfg = config::Config::build();
    let server_config = service::server::Config(cfg.clone());
    let smtp_config = service::smtp::Config(cfg.clone());

    let provider = service::provider::TemplateProvider::from(cfg.clone());
    let smtp_pool = smtp_config.get_pool().expect("smtp service init");

    info!("starting server");
    HttpServer::new(move || {
        bind_services!(
            cfg.clone(),
            create_app!()
                .app_data(Data::new(provider.clone()))
                .app_data(Data::new(smtp_pool.clone()))
                .app_data(Data::new(cfg.clone().render_options()))
                .wrap(DefaultHeaders::new().add(("X-Version", env!("CARGO_PKG_VERSION"))))
                .wrap(Logger::default())
                .wrap(Compress::default())
        )
    })
    .bind(server_config.to_bind())?
    .run()
    .await
}

#[cfg(test)]
#[cfg(not(tarpaulin_include))]
mod tests {
    use super::service::provider::TemplateProvider;
    use crate::config::Config;
    use actix_http::Request;
    use actix_web::dev::ServiceResponse;
    use actix_web::{test, web::Data, App};
    use serde::Deserialize;
    use std::sync::Arc;
    use uuid::Uuid;

    fn env_str(key: &str) -> Option<String> {
        std::env::var(key).ok()
    }

    fn env_number<T: std::str::FromStr>(key: &str) -> Option<T> {
        std::env::var(key)
            .ok()
            .and_then(|value| value.parse::<T>().ok())
    }

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

        fn build_config(&self) -> Arc<Config> {
            let mut opts = vec![];
            if self.authenticated {
                opts.push("--authentication-enabled".to_string());
            }
            opts.push("--smtp-hostname".to_string());
            if self.secure {
                opts.push(SMTPS_HOSTNAME.to_string());
                opts.push("--smtp-port".to_string());
                opts.push(SMTPS_PORT.to_string());
                opts.push("--smtp-tls-enabled".to_string());
                if self.invalid_cert {
                    opts.push("--smtp-accept-invalid-cert".to_string());
                }
            } else {
                opts.push(SMTP_HOSTNAME.to_string());
                opts.push("--smtp-port".to_string());
                opts.push(SMTP_PORT.to_string());
            }
            Config::from_args(opts)
        }

        pub async fn execute(&self, req: Request) -> ServiceResponse {
            let cfg = self.build_config();
            let template_provider = TemplateProvider::from(cfg.clone());
            let smtp_config = crate::service::smtp::Config(cfg.clone());
            let render_opts = cfg.render_options();
            let smtp_pool = smtp_config.get_pool().expect("smtp service init");
            let app = test::init_service(bind_services!(
                cfg.clone(),
                create_app!()
                    .app_data(Data::new(render_opts))
                    .app_data(Data::new(template_provider.clone()))
                    .app_data(Data::new(smtp_pool.clone()))
            ))
            .await;
            test::call_service(&app, req).await
        }
    }

    #[derive(Deserialize)]
    pub struct Email {
        pub html: String,
        pub text: String,
    }

    pub async fn get_latest_inbox(from: &str, to: &str) -> Vec<Email> {
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
