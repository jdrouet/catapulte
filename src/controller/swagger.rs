use actix_http::http::header;
use actix_web::{web, HttpResponse, Responder};
use std::env;

pub const SWAGGER_ENABLED: &str = "SWAGGER_ENABLED";

pub fn config(app: &mut web::ServiceConfig) {
    let enabled = env::var(SWAGGER_ENABLED)
        .map(|value| value == "true")
        .unwrap_or(false);
    if enabled {
        app.service(
            web::scope("")
                .route("/", web::get().to(index))
                .route("/index.html", web::get().to(index))
                .route("/openapi.json", web::get().to(openapi)),
        );
    }
}

async fn index() -> impl Responder {
    HttpResponse::Ok()
        .append_header((header::CONTENT_TYPE, "text/html"))
        .body(include_str!("../../swagger/index.html"))
}

async fn openapi() -> impl Responder {
    HttpResponse::Ok()
        .append_header((header::CONTENT_TYPE, "application/json"))
        .body(include_str!("../../swagger/openapi.json"))
}

// LCOV_EXCL_START
#[cfg(test)]
mod tests {
    use super::SWAGGER_ENABLED;
    use crate::tests::ServerBuilder;
    use actix_web::http::StatusCode;
    use actix_web::test;
    use env_test_util::TempEnvVar;

    #[actix_rt::test]
    #[serial]
    async fn success_enabled() {
        let _swagger = TempEnvVar::new(SWAGGER_ENABLED).with("true");
        let req = test::TestRequest::get().uri("/").to_request();
        let res = ServerBuilder::default().execute(req).await;
        assert_eq!(res.status(), StatusCode::OK);
        let req = test::TestRequest::get().uri("/index.html").to_request();
        let res = ServerBuilder::default().execute(req).await;
        assert_eq!(res.status(), StatusCode::OK);
        let req = test::TestRequest::get().uri("/openapi.json").to_request();
        let res = ServerBuilder::default().execute(req).await;
        assert_eq!(res.status(), StatusCode::OK);
        let req = test::TestRequest::get().uri("/status").to_request();
        let res = ServerBuilder::default().execute(req).await;
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[actix_rt::test]
    #[serial]
    async fn success_disabled() {
        let _swagger = TempEnvVar::new(SWAGGER_ENABLED);
        let req = test::TestRequest::get().uri("/").to_request();
        let res = ServerBuilder::default().execute(req).await;
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        let req = test::TestRequest::get().uri("/index.html").to_request();
        let res = ServerBuilder::default().execute(req).await;
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        let req = test::TestRequest::get().uri("/openapi.json").to_request();
        let res = ServerBuilder::default().execute(req).await;
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        let req = test::TestRequest::get().uri("/status").to_request();
        let res = ServerBuilder::default().execute(req).await;
        assert_eq!(res.status(), StatusCode::OK);
    }
}
