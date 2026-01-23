use axum::extract::Json;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use catapulte_domain::error::{RenderError, SendEmailError, SendError, TemplateLoadError};

/// HTTP error response body
#[derive(Debug, Default, serde::Serialize, utoipa::ToSchema)]
pub struct ErrorResponse {
    #[serde(skip)]
    pub(crate) status: StatusCode,
    pub code: &'static str,
    pub title: &'static str,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub details: Vec<String>,
}

impl IntoResponse for ErrorResponse {
    fn into_response(self) -> axum::response::Response {
        let status = self.status;
        (status, Json(self)).into_response()
    }
}

impl From<SendEmailError> for ErrorResponse {
    fn from(value: SendEmailError) -> Self {
        match value {
            SendEmailError::NoRecipients => ErrorResponse {
                status: StatusCode::BAD_REQUEST,
                code: "no-recipients",
                title: "no recipients specified",
                details: Vec::new(),
            },
            SendEmailError::TemplateLoad(err) => err.into(),
            SendEmailError::Render(err) => err.into(),
            SendEmailError::Send(err) => err.into(),
        }
    }
}

impl From<TemplateLoadError> for ErrorResponse {
    fn from(value: TemplateLoadError) -> Self {
        match value {
            TemplateLoadError::NotFound { name } => ErrorResponse {
                status: StatusCode::NOT_FOUND,
                code: "template-not-found",
                title: "template not found",
                details: vec![format!("Template '{name}' was not found")],
            },
            TemplateLoadError::InvalidMetadata(err) => ErrorResponse {
                status: StatusCode::BAD_GATEWAY,
                code: "invalid-metadata",
                title: "invalid template metadata",
                details: vec![format!("{err}")],
            },
            TemplateLoadError::IoError(err) => ErrorResponse {
                status: StatusCode::BAD_GATEWAY,
                code: "template-io-error",
                title: "failed to read template",
                details: vec![format!("{err}")],
            },
            TemplateLoadError::FetchError(err) => ErrorResponse {
                status: StatusCode::BAD_GATEWAY,
                code: "template-fetch-error",
                title: "failed to fetch remote template",
                details: vec![format!("{err}")],
            },
        }
    }
}

impl From<RenderError> for ErrorResponse {
    fn from(value: RenderError) -> Self {
        match value {
            RenderError::Interpolation(err) => ErrorResponse {
                status: StatusCode::BAD_REQUEST,
                code: "interpolation-error",
                title: "failed to interpolate template variables",
                details: vec![format!("{err}")],
            },
            RenderError::Parse(err) => ErrorResponse {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                code: "template-parse-error",
                title: "failed to parse template",
                details: vec![format!("{err}")],
            },
            RenderError::Render(err) => ErrorResponse {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                code: "template-render-error",
                title: "failed to render template",
                details: vec![format!("{err}")],
            },
        }
    }
}

impl From<SendError> for ErrorResponse {
    fn from(value: SendError) -> Self {
        match value {
            SendError::BuildError(err) => ErrorResponse {
                status: StatusCode::BAD_REQUEST,
                code: "email-build-error",
                title: "failed to build email message",
                details: vec![format!("{err}")],
            },
            SendError::TransportError(err) => {
                tracing::error!("SMTP transport error: {err:?}");
                ErrorResponse {
                    status: StatusCode::BAD_GATEWAY,
                    code: "smtp-transport-error",
                    title: "failed to send email",
                    details: vec![format!("{err}")],
                }
            }
            SendError::ConnectionError(err) => {
                tracing::error!("SMTP connection error: {err:?}");
                ErrorResponse {
                    status: StatusCode::BAD_GATEWAY,
                    code: "smtp-connection-error",
                    title: "failed to connect to email server",
                    details: vec![format!("{err}")],
                }
            }
        }
    }
}

/// Request parsing errors
#[derive(Debug, thiserror::Error)]
pub enum RequestError {
    #[error("invalid email address: {0}")]
    InvalidEmail(String),
    #[error("missing required field: {0}")]
    MissingField(&'static str),
    #[error("invalid field value: {0}")]
    InvalidField(String),
}

impl From<RequestError> for ErrorResponse {
    fn from(value: RequestError) -> Self {
        match value {
            RequestError::InvalidEmail(addr) => ErrorResponse {
                status: StatusCode::BAD_REQUEST,
                code: "invalid-email-address",
                title: "invalid email address",
                details: vec![format!("'{addr}' is not a valid email address")],
            },
            RequestError::MissingField(field) => ErrorResponse {
                status: StatusCode::BAD_REQUEST,
                code: "missing-required-field",
                title: "missing required field",
                details: vec![format!("Field '{field}' is required")],
            },
            RequestError::InvalidField(msg) => ErrorResponse {
                status: StatusCode::BAD_REQUEST,
                code: "invalid-field-value",
                title: "invalid field value",
                details: vec![msg],
            },
        }
    }
}
