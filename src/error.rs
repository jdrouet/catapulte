use axum::extract::Json;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use tokio::time::error::Elapsed;

#[derive(Debug, Default, serde::Serialize, utoipa::ToSchema)]
pub(crate) struct ErrorResponse {
    #[serde(skip)]
    status: StatusCode,
    code: &'static str,
    title: &'static str,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    details: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ServerError {
    #[error("Unable to generate email: {0}")]
    Engine(#[from] catapulte_engine::Error),
    #[error("Unable to perform smtp action: {0}")]
    Smtp(#[from] lettre::error::Error),
    #[error("Unable to perform smtp transport action: {0}")]
    SmtpTransport(#[from] lettre::transport::smtp::Error),
    #[error("Internal timeout error: {0}")]
    Timeout(#[from] Elapsed),
}

impl From<ServerError> for ErrorResponse {
    fn from(value: ServerError) -> Self {
        match value {
            ServerError::Engine(inner) => inner.into(),
            ServerError::Smtp(inner) => inner.into(),
            ServerError::SmtpTransport(inner) => inner.into(),
            ServerError::Timeout(inner) => inner.into(),
        }
    }
}

impl From<lettre::transport::smtp::Error> for ErrorResponse {
    fn from(value: lettre::transport::smtp::Error) -> Self {
        tracing::error!("smtp transport error: {value:?}");
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "smtp-transport-error",
            title: "unable to send message",
            details: vec![format!("{value}")],
        }
    }
}

impl From<Elapsed> for ErrorResponse {
    fn from(_: Elapsed) -> Self {
        ErrorResponse {
            status: StatusCode::GATEWAY_TIMEOUT,
            code: "internal-timeout",
            title: "unable to contact smtp",
            details: Vec::default(),
        }
    }
}

impl From<lettre::error::Error> for ErrorResponse {
    fn from(value: lettre::error::Error) -> Self {
        match value {
            lettre::error::Error::CannotParseFilename => ErrorResponse {
                status: StatusCode::BAD_REQUEST,
                code: "cannot-parse-filename",
                title: "unable to parse attachment filename",
                details: Vec::with_capacity(0),
            },
            lettre::error::Error::EmailMissingAt => ErrorResponse {
                status: StatusCode::BAD_REQUEST,
                code: "invalid-email-address-missing-at",
                title: "unable to find at in email address",
                details: Vec::with_capacity(0),
            },
            lettre::error::Error::EmailMissingDomain => ErrorResponse {
                status: StatusCode::BAD_REQUEST,
                code: "invalid-email-address-missing-domain",
                title: "unable to find domain in email address",
                details: Vec::with_capacity(0),
            },
            lettre::error::Error::EmailMissingLocalPart => ErrorResponse {
                status: StatusCode::BAD_REQUEST,
                code: "invalid-email-address-missing-local-part",
                title: "unable to find local part in email address",
                details: Vec::with_capacity(0),
            },
            lettre::error::Error::Io(inner) => ErrorResponse {
                status: StatusCode::BAD_REQUEST,
                code: "io-error-with-message",
                title: "io error when building message",
                details: vec![format!("{inner:?}")],
            },
            lettre::error::Error::MissingFrom => ErrorResponse {
                status: StatusCode::BAD_REQUEST,
                code: "missing-from-in-message",
                title: "couldn't find from when building message",
                details: Vec::with_capacity(0),
            },
            lettre::error::Error::MissingTo => ErrorResponse {
                status: StatusCode::BAD_REQUEST,
                code: "missing-to-in-message",
                title: "couldn't find to when building message",
                details: Vec::with_capacity(0),
            },
            lettre::error::Error::NonAsciiChars => ErrorResponse {
                status: StatusCode::BAD_REQUEST,
                code: "non-ascii-chars-found",
                title: "couldn't decode strings when building email",
                details: Vec::with_capacity(0),
            },
            lettre::error::Error::TooManyFrom => ErrorResponse {
                status: StatusCode::BAD_REQUEST,
                code: "too-many-from-in-message",
                title: "couldn't define a single from for message",
                details: Vec::with_capacity(0),
            },
        }
    }
}

impl From<catapulte_engine::Error> for ErrorResponse {
    fn from(value: catapulte_engine::Error) -> Self {
        match value {
            catapulte_engine::Error::Building(inner) => inner.into(),
            catapulte_engine::Error::Interpolation(inner) => ErrorResponse {
                status: StatusCode::BAD_REQUEST,
                code: "interpolation-error",
                title: "something went wrong when interpolating values in template",
                details: vec![format!("{inner}")],
            },
            catapulte_engine::Error::Loading(inner) => inner.into(),
            catapulte_engine::Error::Parsing(inner) => inner.into(),
            catapulte_engine::Error::Rendering(inner) => inner.into(),
        }
    }
}

impl From<catapulte_engine::loader::Error> for ErrorResponse {
    fn from(value: catapulte_engine::loader::Error) -> Self {
        match value {
            catapulte_engine::loader::Error::Multiple(inner) => ErrorResponse {
                status: StatusCode::BAD_REQUEST,
                code: "loading-error",
                title: "something went wrong when loading template",
                details: inner.into_iter().map(|v| format!("{v:?}")).collect(),
            },
            catapulte_engine::loader::Error::Http(inner) => inner.into(),
            catapulte_engine::loader::Error::Local(inner) => inner.into(),
        }
    }
}

impl From<catapulte_engine::loader::http::Error> for ErrorResponse {
    fn from(value: catapulte_engine::loader::http::Error) -> Self {
        use catapulte_engine::loader::http::Error;

        match value {
            Error::TemplateLoadingFailed(inner) => ErrorResponse {
                status: StatusCode::BAD_GATEWAY,
                code: "template-loading-failed",
                title: "unable to load template file",
                details: vec![format!("{inner}")],
            },
            Error::MetadataLoadingFailed(inner) => ErrorResponse {
                status: StatusCode::BAD_GATEWAY,
                code: "metadata-loading-failed",
                title: "unable to load metadata file",
                details: vec![format!("{inner}")],
            },
            Error::MetadataUrlInvalid(inner) => ErrorResponse {
                status: StatusCode::BAD_GATEWAY,
                code: "url-building-failed",
                title: "unable to build url",
                details: vec![format!("{inner}")],
            },
            Error::RequestFailed(inner) => ErrorResponse {
                status: StatusCode::BAD_GATEWAY,
                code: "external-request-failed",
                title: "unable to request external resource",
                details: vec![format!("{inner}")],
            },
        }
    }
}

impl From<catapulte_engine::loader::local::Error> for ErrorResponse {
    fn from(value: catapulte_engine::loader::local::Error) -> Self {
        use catapulte_engine::loader::local::Error;

        match value {
            Error::TemplateOpenFailed(inner) => ErrorResponse {
                status: StatusCode::BAD_GATEWAY,
                code: "template-opening-failed",
                title: "unable to open template",
                details: vec![format!("{inner}")],
            },
            Error::MetadataOpenFailed(inner) => ErrorResponse {
                status: StatusCode::BAD_GATEWAY,
                code: "metadata-opening-failed",
                title: "unable to open metadata",
                details: vec![format!("{inner}")],
            },
            Error::MetadataFormatInvalid(inner) => ErrorResponse {
                status: StatusCode::BAD_GATEWAY,
                code: "metadata-invalid-format",
                title: "unable to decode metadata",
                details: vec![format!("{inner}")],
            },
        }
    }
}

impl From<catapulte_engine::parser::Error> for ErrorResponse {
    fn from(value: catapulte_engine::parser::Error) -> Self {
        use catapulte_engine::parser::Error;

        let status = StatusCode::INTERNAL_SERVER_ERROR;

        let (code, title, details) = match value {
            Error::EndOfStream { .. } => (
                "template-format-error",
                "unable to decode template, reached the end early",
                Vec::with_capacity(0),
            ),
            Error::SizeLimit { .. } => (
                "template-size-exceeded",
                "unable to decode template, reached size limit",
                Vec::with_capacity(0),
            ),
            Error::NoRootNode => (
                "template-missing-root",
                "unable to decode template, no root component",
                Vec::with_capacity(0),
            ),
            Error::UnexpectedToken {
                origin: _,
                position,
            } => (
                "template-unexpected-token",
                "unable to decode template, unexpected token",
                vec![format!(
                    "Unexpected token at position {}:{}",
                    position.start, position.end
                )],
            ),
            Error::IncludeLoaderError { .. } => (
                "template-include-loading-error",
                "unable to load included template",
                Vec::with_capacity(0),
            ),
            Error::InvalidAttribute {
                position,
                origin: _,
            } => (
                "template-invalid-attribute",
                "unable to decode template, invalid attribute",
                vec![format!(
                    "Invalid attribute at position {}:{}",
                    position.start, position.end
                )],
            ),
            Error::InvalidFormat {
                position,
                origin: _,
            } => (
                "template-invalid-format",
                "unable to decode template, invalid format",
                vec![format!(
                    "Invalid format at position {}:{}",
                    position.start, position.end
                )],
            ),
            Error::MissingAttribute {
                name,
                position,
                origin: _,
            } => (
                "template-missing-attribute",
                "unable to decode template, missing attribute",
                vec![format!(
                    "Missing attribute {name:?} at position {}:{}",
                    position.start, position.end
                )],
            ),
            Error::ParserError { origin: _, source } => (
                "template-invalid-xml",
                "unable to decode template, invalid xml",
                vec![format!("Parser failed: {source:?}")],
            ),
            Error::UnexpectedElement {
                position,
                origin: _,
            } => (
                "template-unexpected-element",
                "unable to decode template, unexpected element",
                vec![format!(
                    "Unexpected element at position {}:{}",
                    position.start, position.end
                )],
            ),
        };

        ErrorResponse {
            status,
            code,
            title,
            details,
        }
    }
}

impl From<catapulte_engine::render::Error> for ErrorResponse {
    fn from(value: catapulte_engine::render::Error) -> Self {
        use catapulte_engine::render::Error;

        match value {
            Error::Format(_) => ErrorResponse {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                code: "invalid-template-format",
                title: "invalid template format",
                details: Vec::with_capacity(0),
            },
            Error::UnknownFragment(_) => ErrorResponse {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                code: "rendering-unknown-fragment",
                title: "unknown fragment",
                details: Vec::with_capacity(0),
            },
        }
    }
}

impl IntoResponse for ServerError {
    fn into_response(self) -> axum::response::Response {
        let res = ErrorResponse::from(self);
        let status = res.status;
        (status, Json(res)).into_response()
    }
}
