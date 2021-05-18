use actix_http::http;
use actix_web::{get, HttpResponse, Responder};

fn get_index() -> HttpResponse {
    HttpResponse::Ok()
        .append_header((http::header::CONTENT_TYPE, "text/html"))
        .body(include_str!("../../swagger/index.html"))
}

#[get("/")]
pub async fn handle_root() -> impl Responder {
    get_index()
}

#[get("/index.html")]
pub async fn handle_index() -> impl Responder {
    get_index()
}

#[get("/openapi.json")]
pub async fn handle_openapi() -> impl Responder {
    HttpResponse::Ok()
        .append_header((http::header::CONTENT_TYPE, "application/json"))
        .body(include_str!("../../swagger/openapi.json"))
}
