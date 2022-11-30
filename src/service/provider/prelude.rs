use crate::error::ServerError;
use serde_json::error::Error as JsonError;
use std::io::Error as IoError;

#[derive(Clone, Debug)]
pub enum TemplateProviderError {
    MetadataInvalid,
    TemplateNotFound,
}

impl From<IoError> for TemplateProviderError {
    fn from(err: IoError) -> Self {
        metrics::increment_counter!("template_provider_error", "reason" => "template_not_found");
        tracing::debug!("template provider error: template not found ({:?})", err);
        Self::TemplateNotFound
    }
}

impl From<JsonError> for TemplateProviderError {
    fn from(err: JsonError) -> Self {
        metrics::increment_counter!("template_provider_error", "reason" => "metadata_invalid");
        tracing::debug!("template provider error: invalid metadata ({:?})", err);
        Self::MetadataInvalid
    }
}

impl From<TemplateProviderError> for ServerError {
    fn from(err: TemplateProviderError) -> Self {
        match err {
            TemplateProviderError::TemplateNotFound => {
                ServerError::not_found().message("unable to find template")
            }
            TemplateProviderError::MetadataInvalid => {
                ServerError::internal().message("unable to load metadata")
            }
        }
    }
}
