use actix_web::{get, HttpResponse};
use serde_json::json;
use std::time::Instant;

lazy_static! {
    static ref STARTUP: Instant = Instant::now();
}

#[get("/status")]
pub async fn handler() -> HttpResponse {
    HttpResponse::Ok().json(json!({
        "uptime": STARTUP.elapsed().as_secs(),
    }))
}

// LCOV_EXCL_START
#[cfg(test)]
mod tests {
    use crate::tests::execute_request;
    use actix_web::http::StatusCode;
    use actix_web::test;

    #[actix_rt::test]
    #[serial]
    async fn status_success() {
        let req = test::TestRequest::get().uri("/status").to_request();
        let res = execute_request(req).await;
        assert_eq!(res.status(), StatusCode::OK);
    }
}
// LCOV_EXCL_END
