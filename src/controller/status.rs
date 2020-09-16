use crate::service::smtp::SmtpPool;
use actix_web::{get, web, HttpResponse};

#[get("/status")]
pub async fn handler(smtp_pool: web::Data<SmtpPool>) -> HttpResponse {
    match smtp_pool.get() {
        Ok(_) => HttpResponse::NoContent().finish(),
        Err(_) => HttpResponse::InternalServerError().json("Internal Server Error"),
    }
}

#[cfg(not(tarpaulin_include))]
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
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
    }
}
