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

impl std::convert::From<std::io::Error> for ServerError {
    fn from(error: std::io::Error) -> Self {
        metrics::increment_counter!("server_error", "origin" => "std::io::Error");
        tracing::error!("io error: {:?}", error);
        ServerError::internal()
    }
}
