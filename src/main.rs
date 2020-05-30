#[cfg(test)]
#[macro_use]
extern crate serial_test;

#[macro_use]
extern crate log;

use actix_web::{middleware, web, App, HttpServer};
use std::env;

mod controller;
mod error;
mod service;

fn get_address() -> String {
    match env::var("ADDRESS") {
        Ok(value) => value,
        Err(_) => "localhost".into(),
    }
}

fn get_port() -> String {
    match env::var("PORT") {
        Ok(value) => value,
        Err(_) => "3000".into(),
    }
}

fn get_bind() -> String {
    format!("{}:{}", get_address(), get_port())
}

macro_rules! create_app {
    () => {
        App::new().app_data(web::JsonConfig::default().error_handler(error::json_error_handler))
    };
}

macro_rules! bind_services {
    ($app: expr) => {
        $app.service(controller::status::handler)
            .service(controller::template_send_json::handler)
            .service(controller::template_send_multipart::handler)
    };
}

#[actix_rt::main]
async fn main() -> std::io::Result<()> {
    env_logger::init();

    let template_provider =
        service::template::provider::TemplateProvider::from_env().expect("template provider init");
    let smtp_pool = service::smtp::get_pool().expect("smtp service init");

    info!("starting server");
    HttpServer::new(move || {
        bind_services!(create_app!()
            .data(template_provider.clone())
            .data(smtp_pool.clone())
            .wrap(middleware::DefaultHeaders::new().header("X-Version", "0.1.0"))
            .wrap(middleware::Logger::default())
            .wrap(middleware::Compress::default()))
    })
    .bind(get_bind())?
    .run()
    .await
}

#[cfg(test)]
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

    pub async fn get_latest_inbox(from: &String, to: &String) -> Vec<Email> {
        let url = format!("http://localhost:1080/api/emails?from={}&to={}", from, to);
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
        let smtp_pool = service::smtp::get_pool().expect("smtp service init");
        let mut app = test::init_service(bind_services!(create_app!()
            .data(template_provider.clone())
            .data(smtp_pool.clone())))
        .await;
        test::call_service(&mut app, req).await
    }

    #[test]
    #[serial]

    fn test_get_address() {
        std::env::remove_var("ADDRESS");
        assert_eq!(get_address(), "localhost");
        std::env::set_var("ADDRESS", "something");
        assert_eq!(get_address(), "something");
    }

    #[test]
    #[serial]

    fn test_get_port() {
        std::env::remove_var("PORT");
        assert_eq!(get_port(), "3000");
        std::env::set_var("PORT", "1234");
        assert_eq!(get_port(), "1234");
    }

    #[test]
    #[serial]

    fn test_bind() {
        std::env::remove_var("ADDRESS");
        std::env::remove_var("PORT");
        assert_eq!(get_bind(), "localhost:3000");
        std::env::set_var("ADDRESS", "something");
        std::env::set_var("PORT", "1234");
        assert_eq!(get_bind(), "something:1234");
    }
}
