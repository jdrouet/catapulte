use crate::error::ServerError;
use serde_json::json;
use std::borrow::Cow;

#[derive(Clone, Debug)]
pub struct Error {
    pub provider: &'static str,
    pub kind: ErrorKind,
    pub message: Cow<'static, str>,
}

impl Error {
    pub fn not_found(provider: &'static str, message: Cow<'static, str>) -> Self {
        Self {
            provider,
            kind: ErrorKind::NotFound,
            message,
        }
    }

    pub fn configuration(provider: &'static str, message: Cow<'static, str>) -> Self {
        Self {
            provider,
            kind: ErrorKind::Configuration,
            message,
        }
    }

    pub fn internal(provider: &'static str, message: Cow<'static, str>) -> Self {
        Self {
            provider,
            kind: ErrorKind::Internal,
            message,
        }
    }

    pub fn provider(provider: &'static str, message: Cow<'static, str>) -> Self {
        Self {
            provider,
            kind: ErrorKind::Provider,
            message,
        }
    }
}

#[derive(Clone, Debug)]
pub enum ErrorKind {
    NotFound,
    Provider,
    Configuration,
    Internal,
}

impl From<Error> for ServerError {
    fn from(err: Error) -> Self {
        match err.kind {
            ErrorKind::NotFound => ServerError::not_found(),
            ErrorKind::Provider => ServerError::failed_dependency(),
            ErrorKind::Configuration => ServerError::internal(),
            ErrorKind::Internal => ServerError::internal(),
        }
        .message("unable to prepare template")
        .details(json!({ "provider": err.provider, "details": err.message }))
    }
}
