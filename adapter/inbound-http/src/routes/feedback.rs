use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use catapulte_domain::use_case::ingest_feedback::IngestFeedbackUseCase;

use crate::HttpServerState;
use crate::error::AppError;
use crate::feedback::parse_report;

/// Takes a forwarded DSN or abuse report as a raw `message/rfc822` body.
///
/// # Errors
///
/// Returns `AppError::Rejected` (422) when the message is not a usable report or cannot be
/// tied to an email, and 500 when storage or publishing fails.
#[tracing::instrument(skip_all)]
pub async fn ingest_feedback<S: HttpServerState>(
    State(state): State<S>,
    body: Bytes,
) -> Result<StatusCode, AppError> {
    let report = parse_report(&body).map_err(|e| {
        tracing::info!(reason = %e, "feedback report rejected");
        AppError::Rejected
    })?;
    match state.ingest_feedback().execute(report).await {
        Ok(()) => Ok(StatusCode::NO_CONTENT),
        Err(e) if e.is_rejection() => {
            tracing::info!(reason = %e, "feedback report rejected");
            Err(AppError::Rejected)
        }
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
pub(crate) struct NoopIngestFeedback;

#[cfg(test)]
impl IngestFeedbackUseCase for NoopIngestFeedback {
    async fn execute(
        &self,
        _: catapulte_domain::use_case::ingest_feedback::FeedbackReport,
    ) -> Result<(), catapulte_domain::use_case::ingest_feedback::IngestFeedbackError> {
        Ok(())
    }
}
