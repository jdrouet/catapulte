use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use catapulte_domain::use_case::ingest_feedback::IngestFeedbackError;
use catapulte_domain::use_case::list_emails::ListEmailsError;
use catapulte_domain::use_case::list_events::ListEventsError;
use catapulte_domain::use_case::list_senders::ListSendersError;
use catapulte_domain::use_case::manage_suppressions::ManageSuppressionsError;
use catapulte_domain::use_case::manage_templates::ManageTemplatesError;
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
    #[error(transparent)]
    ManageSuppressions(#[from] ManageSuppressionsError),
    #[error(transparent)]
    Feedback(#[from] IngestFeedbackError),
    #[error(transparent)]
    Templates(#[from] ManageTemplatesError),
    #[error("internal error")]
    Internal(#[source] anyhow::Error),
    #[error("feedback report rejected")]
    Rejected,
    #[error("admin key required")]
    Forbidden,
    #[error("not found")]
    NotFound,
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
        if let Self::Templates(ManageTemplatesError::Invalid { message }) = &self {
            tracing::info!(reason = %message, "template rejected");
            return (StatusCode::BAD_REQUEST, Json(ErrorBody { error: message })).into_response();
        }
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
            Self::Templates(ManageTemplatesError::OnDisk { .. }) => (
                StatusCode::CONFLICT,
                "the template is a file on disk and cannot be changed here",
            ),
            Self::Templates(ManageTemplatesError::NotFound { .. }) => {
                (StatusCode::NOT_FOUND, "not found")
            }
            Self::Rejected => (StatusCode::UNPROCESSABLE_ENTITY, "report not accepted"),
            Self::Forbidden => (StatusCode::FORBIDDEN, "admin key required"),
            Self::NotFound => (StatusCode::NOT_FOUND, "not found"),
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
            | Self::ListSenders(_)
            | Self::ManageSuppressions(_)
            | Self::Feedback(_)
            | Self::Internal(_)
            | Self::Templates(
                ManageTemplatesError::Invalid { .. }
                | ManageTemplatesError::Repository(_)
                | ManageTemplatesError::Resolve(_),
            ) => (StatusCode::INTERNAL_SERVER_ERROR, "internal error"),
        };
        tracing::error!(error = ?self, status = %status.as_u16(), "request failed");
        (status, Json(ErrorBody { error: message })).into_response()
    }
}
