use axum::extract::Json;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde_json::Value as JsonValue;

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

impl From<catapulte_engine::Error> for ServerError {
    fn from(value: catapulte_engine::Error) -> Self {
        Self {
            code: StatusCode::INTERNAL_SERVER_ERROR,
            message: "unable to prepare email",
            details: Some(serde_json::json!({
                "message": format!("{value}"),
            })),
        }
    }
}
