#![allow(clippy::enum_variant_names)]

use axum::extract::Json;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde_json::Value as JsonValue;
use std::borrow::Cow;

#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
pub(crate) struct ServerError {
    #[serde(skip)]
    code: StatusCode,
    pub message: Cow<'static, str>,
    #[schema(value_type = Object)]
    pub details: Option<JsonValue>,
}

impl ServerError {
    pub(crate) fn internal() -> Self {
        Self {
            code: StatusCode::INTERNAL_SERVER_ERROR,
            message: Cow::Borrowed(StatusCode::INTERNAL_SERVER_ERROR.as_str()),
            details: None,
        }
    }

    pub(crate) fn bad_request(message: String) -> Self {
        Self {
            code: StatusCode::BAD_REQUEST,
            message: Cow::Owned(message),
            details: None,
        }
    }

    pub(crate) fn not_found() -> Self {
        Self {
            code: StatusCode::BAD_REQUEST,
            message: Cow::Borrowed("resource not found"),
            details: None,
        }
    }

    pub(crate) fn message(mut self, message: Cow<'static, str>) -> Self {
        self.message = message;
        self
    }

    pub(crate) fn details(mut self, details: serde_json::Value) -> Self {
        self.details = Some(details);
        self
    }
}

impl IntoResponse for ServerError {
    fn into_response(self) -> axum::response::Response {
        (self.code, Json(self)).into_response()
    }
}

// impl ServerError {
//     fn response(&self) -> HttpResponseBuilder {
//         match *self {
//             ServerError::BadRequest(_) => HttpResponse::BadRequest(),
//             ServerError::NotFound(_) => HttpResponse::NotFound(),
//             ServerError::InternalServerError(_) => HttpResponse::InternalServerError(),
//             ServerError::Unauthorized(_) => HttpResponse::Unauthorized(),
//         }
//     }

//     fn name(&self) -> String {
//         match *self {
//             ServerError::BadRequest(_) => "Bad Request".into(),
//             ServerError::NotFound(_) => "Not Found".into(),
//             ServerError::InternalServerError(_) => "Internal Server Error".into(),
//             ServerError::Unauthorized(_) => "Unauthorized".into(),
//         }
//     }

//     fn message(&self) -> String {
//         match *self {
//             ServerError::BadRequest(ref msg) => msg.clone(),
//             ServerError::NotFound(ref msg) => msg.clone(),
//             ServerError::InternalServerError(ref msg) => msg.clone(),
//             ServerError::Unauthorized(ref msg) => msg.clone(),
//         }
//     }
// }

// impl ResponseError for ServerError {
//     fn error_response(&self) -> HttpResponse {
//         let response = ServerErrorResponse {
//             name: self.name(),
//             message: Some(self.message()),
//         };
//         self.response().json(&response)
//     }
// }

// impl From<actix_web::error::JsonPayloadError> for ServerError {
//     fn from(error: actix_web::error::JsonPayloadError) -> Self {
//         match error {
//             actix_web::error::JsonPayloadError::Deserialize(err) => {
//                 ServerError::BadRequest(err.to_string())
//             }
//             _ => ServerError::BadRequest(error.to_string()),
//         }
//     }
// }

// impl std::convert::From<r2d2::Error> for ServerError {
//     fn from(error: r2d2::Error) -> Self {
//         eprintln!("r2d2 pool error: {:?}", error);
//         // tracing::error!("r2d2 pool error", {
//         //     error: format!("{:?}", error),
//         // });
//         ServerError::internal()
//     }
// }

// pub fn json_error_handler(
//     err: actix_web::error::JsonPayloadError,
//     _req: &HttpRequest,
// ) -> actix_web::error::Error {
//     error!("json_error_handler: {:?}", err);
//     let error = ServerError::from(err);
//     let res = error.error_response();
//     actix_web::error::InternalError::from_response(error, res).into()
// }

impl std::convert::From<std::io::Error> for ServerError {
    fn from(error: std::io::Error) -> Self {
        eprintln!("std io error: {:?}", error);
        // tracing::error!("io error", {
        //     error: format!("{:?}", error),
        // });
        ServerError::internal()
    }
}
