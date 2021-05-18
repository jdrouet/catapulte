use super::template::Template;
use crate::error::ServerError;
use async_trait::async_trait;
use serde_json::error::Error as JsonError;
use std::io::Error as IoError;

#[derive(Clone, Debug)]
pub enum TemplateManagerError {
    MetadataInvalid,
    InternalError(String),
    TemplateNotFound,
}

impl From<IoError> for TemplateManagerError {
    fn from(_err: IoError) -> Self {
        TemplateManagerError::TemplateNotFound
    }
}

impl From<JsonError> for TemplateManagerError {
    fn from(_err: JsonError) -> Self {
        TemplateManagerError::MetadataInvalid
    }
}

impl From<reqwest::Error> for TemplateManagerError {
    fn from(err: reqwest::Error) -> Self {
        match err.status() {
            Some(reqwest::StatusCode::NOT_FOUND) => Self::TemplateNotFound,
            _ => Self::InternalError(format!("network error {:?}", err)),
        }
    }
}

impl From<TemplateManagerError> for ServerError {
    fn from(err: TemplateManagerError) -> Self {
        match err {
            TemplateManagerError::TemplateNotFound => {
                ServerError::NotFound("unable to find template".into())
            }
            TemplateManagerError::MetadataInvalid => {
                ServerError::InternalServerError("unable to load metadata".into())
            }
            TemplateManagerError::InternalError(msg) => ServerError::InternalServerError(msg),
        }
    }
}

#[async_trait]
pub trait TemplateManager {
    async fn find_by_name(&self, name: &str) -> Result<Template, TemplateManagerError>;
}
