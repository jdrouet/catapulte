#[cfg(test)]
#[macro_use]
extern crate serial_test;

#[macro_use]
extern crate log;

use actix_web::{middleware, App, HttpServer};
use std::env;

mod controller;

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

macro_rules! bind_services {
    ($app: expr) => {
        $app.service(controller::status::handler)
    };
}

#[actix_rt::main]
async fn main() -> std::io::Result<()> {
    env_logger::init();
    info!("starting server");
    HttpServer::new(|| bind_services!(App::new().wrap(middleware::Logger::default())))
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

    pub async fn execute_request(req: Request) -> ServiceResponse {
        let mut app = test::init_service(bind_services!(App::new())).await;
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
