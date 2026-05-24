use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use catapulte_domain::port::event_repository::EventRepositoryError;
use catapulte_domain::use_case::submit_email::SubmitEmailError;

use crate::dto::BodyConversionError;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error(transparent)]
    BadRequest(#[from] BodyConversionError),
    #[error(transparent)]
    Submit(#[from] SubmitEmailError),
    #[error(transparent)]
    ListEvents(#[from] EventRepositoryError),
    #[error("invalid email id")]
    InvalidEmailId,
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    error: &'a str,
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, message) = match &self {
            Self::BadRequest(_) | Self::InvalidEmailId => {
                (StatusCode::BAD_REQUEST, "invalid request")
            }
            Self::Submit(
                SubmitEmailError::Persist(_)
                | SubmitEmailError::Enqueue(_)
                | SubmitEmailError::Publish(_),
            )
            | Self::ListEvents(_) => (StatusCode::INTERNAL_SERVER_ERROR, "internal error"),
        };
        tracing::error!(error = ?self, status = %status.as_u16(), "request failed");
        (status, Json(ErrorBody { error: message })).into_response()
    }
}
