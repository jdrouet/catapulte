use axum::extract::Json;
use axum::http::StatusCode;
use axum::response::IntoResponse;

#[derive(Debug, Default, serde::Serialize)]
pub(crate) struct ErrorResponse {
    #[serde(skip)]
    status: StatusCode,
    code: &'static str,
    title: &'static str,
    #[serde(default)]
    details: Option<String>,
}

impl<'s> utoipa::ToSchema<'s> for ErrorResponse {
    fn schema() -> (
        &'s str,
        utoipa::openapi::RefOr<utoipa::openapi::schema::Schema>,
    ) {
        (
            "ServerError",
            utoipa::openapi::ObjectBuilder::new()
                .property(
                    "code",
                    utoipa::openapi::ObjectBuilder::new()
                        .schema_type(utoipa::openapi::SchemaType::String),
                )
                .required("code")
                .property(
                    "title",
                    utoipa::openapi::ObjectBuilder::new()
                        .schema_type(utoipa::openapi::SchemaType::String),
                )
                .required("title")
                .property(
                    "details",
                    utoipa::openapi::ObjectBuilder::new()
                        .schema_type(utoipa::openapi::SchemaType::String),
                )
                .into(),
        )
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ServerError {
    #[error("Unable to generate email: {0}")]
    Engine(#[from] catapulte_engine::Error),
    #[error("Unable to perform smtp action: {0}")]
    Smtp(#[from] lettre::error::Error),
    #[error("Unable to perform smtp transport action: {0}")]
    SmtpTransport(#[from] lettre::transport::smtp::Error),
}

impl From<ServerError> for ErrorResponse {
    fn from(value: ServerError) -> Self {
        match value {
            ServerError::Engine(inner) => inner.into(),
            ServerError::Smtp(inner) => inner.into(),
            ServerError::SmtpTransport(inner) => inner.into(),
        }
    }
}

impl From<lettre::transport::smtp::Error> for ErrorResponse {
    fn from(value: lettre::transport::smtp::Error) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "smtp-transport-error",
            title: "unable to send message",
            details: Some(format!("{value}")),
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
                details: None,
            },
            lettre::error::Error::EmailMissingAt => ErrorResponse {
                status: StatusCode::BAD_REQUEST,
                code: "invalid-email-address-missing-at",
                title: "unable to find at in email address",
                details: None,
            },
            lettre::error::Error::EmailMissingDomain => ErrorResponse {
                status: StatusCode::BAD_REQUEST,
                code: "invalid-email-address-missing-domain",
                title: "unable to find domain in email address",
                details: None,
            },
            lettre::error::Error::EmailMissingLocalPart => ErrorResponse {
                status: StatusCode::BAD_REQUEST,
                code: "invalid-email-address-missing-local-part",
                title: "unable to find local part in email address",
                details: None,
            },
            lettre::error::Error::Io(inner) => ErrorResponse {
                status: StatusCode::BAD_REQUEST,
                code: "io-error-with-message",
                title: "io error when building message",
                details: Some(format!("{inner:?}")),
            },
            lettre::error::Error::MissingFrom => ErrorResponse {
                status: StatusCode::BAD_REQUEST,
                code: "missing-from-in-message",
                title: "couldn't find from when building message",
                details: None,
            },
            lettre::error::Error::MissingTo => ErrorResponse {
                status: StatusCode::BAD_REQUEST,
                code: "missing-to-in-message",
                title: "couldn't find to when building message",
                details: None,
            },
            lettre::error::Error::NonAsciiChars => ErrorResponse {
                status: StatusCode::BAD_REQUEST,
                code: "non-ascii-chars-found",
                title: "couldn't decode strings when building email",
                details: None,
            },
            lettre::error::Error::TooManyFrom => ErrorResponse {
                status: StatusCode::BAD_REQUEST,
                code: "too-many-from-in-message",
                title: "couldn't define a single from for message",
                details: None,
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
                details: Some(format!("{inner}")),
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
                details: Some(format!("{inner}")),
            },
            Error::MetadataLoadingFailed(inner) => ErrorResponse {
                status: StatusCode::BAD_GATEWAY,
                code: "metadata-loading-failed",
                title: "unable to load metadata file",
                details: Some(format!("{inner}")),
            },
            Error::MetadataUrlInvalid(inner) => ErrorResponse {
                status: StatusCode::BAD_GATEWAY,
                code: "url-building-failed",
                title: "unable to build url",
                details: Some(format!("{inner}")),
            },
            Error::RequestFailed(inner) => ErrorResponse {
                status: StatusCode::BAD_GATEWAY,
                code: "external-request-failed",
                title: "unable to request external resource",
                details: Some(format!("{inner}")),
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
                details: Some(format!("{inner}")),
            },
            Error::MetadataOpenFailed(inner) => ErrorResponse {
                status: StatusCode::BAD_GATEWAY,
                code: "metadata-opening-failed",
                title: "unable to open metadata",
                details: Some(format!("{inner}")),
            },
            Error::MetadataFormatInvalid(inner) => ErrorResponse {
                status: StatusCode::BAD_GATEWAY,
                code: "metadata-invalid-format",
                title: "unable to decode metadata",
                details: Some(format!("{inner}")),
            },
        }
    }
}

impl From<catapulte_engine::parser::Error> for ErrorResponse {
    fn from(value: catapulte_engine::parser::Error) -> Self {
        use catapulte_engine::parser::Error;

        let status = StatusCode::INTERNAL_SERVER_ERROR;

        let (code, title, details) = match value {
            Error::EndOfStream => (
                "template-format-error",
                "unable to decode template, reached the end early",
                None,
            ),
            Error::SizeLimit => (
                "template-size-exceeded",
                "unable to decode template, reached size limit",
                None,
            ),
            Error::NoRootNode => (
                "template-missing-root",
                "unable to decode template, no root component",
                None,
            ),
            Error::UnexpectedToken(span) => (
                "template-unexpected-token",
                "unable to decode template, unexpected token",
                Some(format!(
                    "Unexpected token at position {}:{}",
                    span.start, span.end
                )),
            ),
            Error::IncludeLoaderError { .. } => (
                "template-include-loading-error",
                "unable to load included template",
                None,
            ),
            Error::InvalidAttribute(span) => (
                "template-invalid-attribute",
                "unable to decode template, invalid attribute",
                Some(format!(
                    "Invalid attribute at position {}:{}",
                    span.start, span.end
                )),
            ),
            Error::InvalidFormat(span) => (
                "template-invalid-format",
                "unable to decode template, invalid format",
                Some(format!(
                    "Invalid format at position {}:{}",
                    span.start, span.end
                )),
            ),
            Error::MissingAttribute(name, span) => (
                "template-missing-attribute",
                "unable to decode template, missing attribute",
                Some(format!(
                    "Missing attribute {name:?} at position {}:{}",
                    span.start, span.end
                )),
            ),
            Error::ParserError(inner) => (
                "template-invalid-xml",
                "unable to decode template, invalid xml",
                Some(format!("Parser failed: {inner:?}")),
            ),
            Error::UnexpectedAttribute(span) => (
                "template-unexpected-attribute",
                "unable to decode template, unexpected attribute",
                Some(format!(
                    "Unexpected attribute at position {}:{}",
                    span.start, span.end
                )),
            ),
            Error::UnexpectedElement(span) => (
                "template-unexpected-element",
                "unable to decode template, unexpected element",
                Some(format!(
                    "Unexpected element at position {}:{}",
                    span.start, span.end
                )),
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
            Error::UnknownFragment(_) => ErrorResponse {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                code: "rendering-unknown-fragment",
                title: "unknown fragment",
                details: None,
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
