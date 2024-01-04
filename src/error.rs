use axum::extract::Json;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde_json::{json, Value as JsonValue};

#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
pub(crate) struct ServerError {
    #[serde(skip)]
    pub code: StatusCode,
    pub message: &'static str,
    #[schema(value_type = Object)]
    pub details: Option<JsonValue>,
}

impl ServerError {
    pub(crate) fn internal() -> Self {
        Self {
            code: StatusCode::INTERNAL_SERVER_ERROR,
            message: StatusCode::INTERNAL_SERVER_ERROR.as_str(),
            details: None,
        }
    }

    pub(crate) fn bad_request(message: &'static str) -> Self {
        Self {
            code: StatusCode::BAD_REQUEST,
            message,
            details: None,
        }
    }

    pub(crate) fn not_found() -> Self {
        Self {
            code: StatusCode::BAD_REQUEST,
            message: "resource not found",
            details: None,
        }
    }

    pub(crate) fn failed_dependency() -> Self {
        Self {
            code: StatusCode::FAILED_DEPENDENCY,
            message: "external service failed",
            details: None,
        }
    }

    pub(crate) fn message(mut self, message: &'static str) -> Self {
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
        metrics::counter!("server_error", "origin" => "std::io::Error").increment(1);
        tracing::error!("io error: {:?}", error);
        ServerError::internal().details(json!({ "origin": "io" }))
    }
}
