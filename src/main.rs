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
        $app.configure(controller::swagger::config)
            .configure(controller::templates::config)
            .service(controller::status::handler)
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
    use super::*;
    use actix_http::Request;
    use actix_web::dev::ServiceResponse;
    use actix_web::{test, App};
    use reqwest;
    use serde::Deserialize;
    use uuid::Uuid;

    #[derive(Deserialize)]
    pub struct Email {
        pub html: String,
        pub text: String,
    }

    fn get_inbox_hostname() -> String {
        std::env::var("INBOX_HOSTNAME").unwrap_or("127.0.0.1".into())
    }

    fn get_inbox_port() -> String {
        std::env::var("INBOX_PORT").unwrap_or("1080".into())
    }

    pub async fn get_latest_inbox(from: &String, to: &String) -> Vec<Email> {
        let url = format!(
            "http://{}:{}/api/emails?from={}&to={}",
            get_inbox_hostname(),
            get_inbox_port(),
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

    pub async fn execute_request(req: Request) -> ServiceResponse {
        let template_provider = service::template::provider::TemplateProvider::from_env()
            .expect("template provider init");
        let smtp_pool = service::smtp::Config::from_env()
            .get_pool()
            .expect("smtp service init");
        let mut app = test::init_service(bind_services!(create_app!()
            .data(template_provider.clone())
            .data(smtp_pool.clone())))
        .await;
        test::call_service(&mut app, req).await
    }

    #[test]
    #[serial]

    fn test_get_address() {
        let _address = env_test_util::TempEnvVar::new("ADDRESS");
        assert_eq!(Config::from_env().address, "127.0.0.1");
        let _address = _address.with("something");
        assert_eq!(Config::from_env().address, "something");
    }

    #[test]
    #[serial]

    fn test_get_port() {
        let _port = env_test_util::TempEnvVar::new("PORT");
        assert_eq!(Config::from_env().port, "3000");
        let _port = _port.with("1234");
        assert_eq!(Config::from_env().port, "1234");
    }

    #[test]
    #[serial]
    fn test_bind() {
        let _address = env_test_util::TempEnvVar::new("ADDRESS");
        let _port = env_test_util::TempEnvVar::new("PORT");
        assert_eq!(Config::from_env().to_bind(), "127.0.0.1:3000");
        let _address = _address.with("something");
        let _port = _port.with("1234");
        assert_eq!(Config::from_env().to_bind(), "something:1234");
    }
}
