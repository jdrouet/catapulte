use super::template::Template;
use crate::error::ServerError;
use serde_json::error::Error as JsonError;
use std::io::Error as IoError;

#[derive(Debug, PartialEq)]
pub enum TemplateManagerError {
    TemplateNotFound,
    MetadataInvalid,
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

impl std::fmt::Display for TemplateManagerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
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
        }
    }
}

pub trait TemplateManager {
    fn find_by_name(&self, name: &str) -> Result<Template, TemplateManagerError>;
}
