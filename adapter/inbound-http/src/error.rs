use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use catapulte_domain::use_case::list_emails::ListEmailsError;
use catapulte_domain::use_case::list_events::ListEventsError;
use catapulte_domain::use_case::list_senders::ListSendersError;
use catapulte_domain::use_case::submit_email::SubmitEmailError;

use crate::dto::EnvelopeConversionError;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error(transparent)]
    BadRequest(#[from] EnvelopeConversionError),
    #[error("bad request: {0}")]
    BadRequestRaw(String),
    #[error(transparent)]
    JsonBody(#[from] axum::extract::rejection::JsonRejection),
    #[error(transparent)]
    Submit(#[from] SubmitEmailError),
    #[error(transparent)]
    ListEmails(#[from] ListEmailsError),
    #[error(transparent)]
    ListEvents(#[from] ListEventsError),
    #[error(transparent)]
    ListSenders(#[from] ListSendersError),
    #[error("invalid email id")]
    InvalidEmailId,
    #[error("invalid error_class value")]
    InvalidErrorClass,
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    error: &'a str,
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, message) = match &self {
            // A body that does not deserialize is a client error (400), like the single
            // endpoint; other rejections (content type, size) keep their own status.
            Self::JsonBody(JsonRejection::JsonDataError(_)) => {
                (StatusCode::BAD_REQUEST, "invalid request")
            }
            Self::JsonBody(other) => (other.status(), "invalid request"),
            Self::BadRequest(_)
            | Self::BadRequestRaw(_)
            | Self::InvalidEmailId
            | Self::InvalidErrorClass
            | Self::Submit(
                SubmitEmailError::AttachmentFetch { .. } | SubmitEmailError::MessageTooLarge { .. },
            ) => (StatusCode::BAD_REQUEST, "invalid request"),
            Self::Submit(SubmitEmailError::SenderDomainNotAllowed { .. }) => {
                (StatusCode::FORBIDDEN, "sender domain is not allowed")
            }
            Self::Submit(SubmitEmailError::AllRecipientsSuppressed) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "every recipient is on the suppression list",
            ),
            Self::Submit(SubmitEmailError::ReservationIncomplete { .. }) => (
                StatusCode::CONFLICT,
                "submission with this idempotency key is incomplete; retry later",
            ),
            Self::Submit(
                SubmitEmailError::Persist(_)
                | SubmitEmailError::Enqueue(_)
                | SubmitEmailError::AttachmentStore { .. }
                | SubmitEmailError::Suppression(_),
            )
            | Self::ListEmails(_)
            | Self::ListEvents(_)
            | Self::ListSenders(_) => (StatusCode::INTERNAL_SERVER_ERROR, "internal error"),
        };
        tracing::error!(error = ?self, status = %status.as_u16(), "request failed");
        (status, Json(ErrorBody { error: message })).into_response()
    }
}
