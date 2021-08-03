#![allow(clippy::enum_variant_names)]

use actix_http::ResponseBuilder;
use actix_web::error::ResponseError;
use actix_web::{HttpRequest, HttpResponse};
use serde::Serialize;
use std::fmt::Display;
use std::fmt::Formatter;

#[derive(Debug, Serialize)]
pub struct ServerErrorResponse {
    pub name: String,
    pub message: Option<String>,
}

#[derive(Debug)]
pub enum ServerError {
    BadRequest(String),
    NotFound(String),
    InternalServerError(String),
    Unauthorized(String),
}

impl Display for ServerError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "InternalServerError")
    }
}

impl ServerError {
    fn response(&self) -> ResponseBuilder {
        match *self {
            ServerError::BadRequest(_) => HttpResponse::BadRequest(),
            ServerError::NotFound(_) => HttpResponse::NotFound(),
            ServerError::InternalServerError(_) => HttpResponse::InternalServerError(),
            ServerError::Unauthorized(_) => HttpResponse::Unauthorized(),
        }
    }

    fn name(&self) -> String {
        match *self {
            ServerError::BadRequest(_) => "Bad Request".into(),
            ServerError::NotFound(_) => "Not Found".into(),
            ServerError::InternalServerError(_) => "Internal Server Error".into(),
            ServerError::Unauthorized(_) => "Unauthorized".into(),
        }
    }

    fn message(&self) -> String {
        match *self {
            ServerError::BadRequest(ref msg) => msg.clone(),
            ServerError::NotFound(ref msg) => msg.clone(),
            ServerError::InternalServerError(ref msg) => msg.clone(),
            ServerError::Unauthorized(ref msg) => msg.clone(),
        }
    }
}

impl ResponseError for ServerError {
    fn error_response(&self) -> HttpResponse {
        let response = ServerErrorResponse {
            name: self.name(),
            message: Some(self.message()),
        };
        self.response().json(&response)
    }
}

impl From<actix_web::error::JsonPayloadError> for ServerError {
    fn from(error: actix_web::error::JsonPayloadError) -> Self {
        match error {
            actix_web::error::JsonPayloadError::Deserialize(err) => {
                ServerError::BadRequest(err.to_string())
            }
            _ => ServerError::BadRequest(error.to_string()),
        }
    }
}

impl std::convert::From<r2d2::Error> for ServerError {
    fn from(error: r2d2::Error) -> Self {
        ServerError::InternalServerError(error.to_string())
    }
}

pub fn json_error_handler(
    err: actix_web::error::JsonPayloadError,
    _req: &HttpRequest,
) -> actix_web::error::Error {
    error!("json_error_handler: {:?}", err);
    let error = ServerError::from(err);
    let res = error.error_response();
    actix_web::error::InternalError::from_response(error, res).into()
}

impl std::convert::From<std::io::Error> for ServerError {
    fn from(error: std::io::Error) -> Self {
        ServerError::InternalServerError(error.to_string())
    }
}
