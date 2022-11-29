use crate::error::ServerError;
use serde_json::error::Error as JsonError;
use std::borrow::Cow;
use std::io::Error as IoError;

#[derive(Clone, Debug)]
pub enum TemplateProviderError {
    MetadataInvalid,
    // InternalError(String),
    TemplateNotFound,
}

impl From<IoError> for TemplateProviderError {
    fn from(_err: IoError) -> Self {
        Self::TemplateNotFound
    }
}

impl From<JsonError> for TemplateProviderError {
    fn from(_err: JsonError) -> Self {
        Self::MetadataInvalid
    }
}

impl From<TemplateProviderError> for ServerError {
    fn from(err: TemplateProviderError) -> Self {
        match err {
            TemplateProviderError::TemplateNotFound => {
                ServerError::not_found().message(Cow::Borrowed("unable to find template"))
            }
            TemplateProviderError::MetadataInvalid => {
                ServerError::internal().message(Cow::Borrowed("unable to load metadata"))
            }
        }
    }
}
