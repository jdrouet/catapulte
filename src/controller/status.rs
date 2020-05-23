use actix_web::{get, HttpResponse};

#[get("/status")]
pub async fn handler() -> HttpResponse {
    HttpResponse::NoContent().finish()
}

#[cfg(test)]
mod tests {
    use crate::tests::execute_request;
    use actix_web::http::StatusCode;
    use actix_web::test;


    #[actix_rt::test]
    #[serial]
    async fn status_get_success() {
        let req = test::TestRequest::get().uri("/status").to_request();
        let res = execute_request(req).await;
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
    }

}
