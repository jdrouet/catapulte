use crate::error::ServerError;
use crate::service::template::Template;
use async_trait::async_trait;
use serde_json::error::Error as JsonError;
use std::io::Error as IoError;

#[derive(Clone, Debug)]
pub enum TemplateProviderError {
    MetadataInvalid,
    InternalError(String),
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

impl From<reqwest::Error> for TemplateProviderError {
    fn from(err: reqwest::Error) -> Self {
        match err.status() {
            Some(reqwest::StatusCode::NOT_FOUND) => Self::TemplateNotFound,
            _ => Self::InternalError(format!("network error {:?}", err)),
        }
    }
}

impl From<TemplateProviderError> for ServerError {
    fn from(err: TemplateProviderError) -> Self {
        match err {
            TemplateProviderError::TemplateNotFound => {
                ServerError::NotFound("unable to find template".into())
            }
            TemplateProviderError::MetadataInvalid => {
                ServerError::InternalServerError("unable to load metadata".into())
            }
            TemplateProviderError::InternalError(msg) => ServerError::InternalServerError(msg),
        }
    }
}

#[async_trait]
pub trait TemplateProvider {
    async fn find_by_name(&self, name: &str) -> Result<Template, TemplateProviderError>;
}
