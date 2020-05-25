use crate::service::smtp::SmtpError;
use actix_web::error::ResponseError;
use actix_web::HttpResponse;
use std::fmt::Display;

#[derive(Debug)]
pub enum ServerError {
    Smtp(SmtpError),
}

impl ResponseError for ServerError {
    fn error_response(&self) -> HttpResponse {
        HttpResponse::InternalServerError().json("Internal Server Error")
    }
}

impl Display for ServerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "InternalServerError")
    }
}
