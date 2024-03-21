use super::prelude::Template;
use serde::Deserialize;
use serde_json::Value as JsonValue;
use std::fs::{read_to_string, File};
use std::io::BufReader;
use std::path::PathBuf;

#[derive(Clone, Debug, serde::Deserialize)]
pub struct Config {
    pub path: PathBuf,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            path: PathBuf::new().join("template"),
        }
    }
}

impl From<Config> for LocalLoader {
    fn from(value: Config) -> Self {
        Self::new(value.path)
    }
}

fn default_template_path() -> String {
    "template.mjml".into()
}

#[derive(Debug, Deserialize)]
pub struct LocalMetadata {
    name: String,
    description: String,
    #[serde(default = "default_template_path")]
    template: String,
    attributes: JsonValue,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Unable to open metadata file: {0:?}")]
    MetadataOpenFailed(std::io::Error),
    #[error("Unable to deserialize metadata file: {0:?}")]
    MetadataFormatInvalid(serde_json::Error),
    #[error("Unable to open template file: {0:?}")]
    TemplateOpenFailed(std::io::Error),
}

#[derive(Debug)]
pub struct LocalLoader {
    root: PathBuf,
}

impl LocalLoader {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

impl LocalLoader {
    pub(super) async fn find_by_name(&self, name: &str) -> Result<Template, Error> {
        tracing::debug!("loading template {}", name);
        let path = self.root.join(name).join("metadata.json");
        let metadata_file = File::open(path).map_err(|err| {
            metrics::counter!("template_provider_error", "reason" => "metadata_not_found")
                .increment(1);
            tracing::debug!("template provider error: metadata not found ({:?})", err);
            Error::MetadataOpenFailed(err)
        })?;
        let metadata_reader = BufReader::new(metadata_file);
        let metadata: LocalMetadata = serde_json::from_reader(metadata_reader).map_err(|err| {
            metrics::counter!("template_provider_error", "reason" => "metadata_invalid")
                .increment(1);
            tracing::debug!("template provider error: metadata invalid ({:?})", err);
            Error::MetadataFormatInvalid(err)
        })?;
        let template_path = self.root.join(name).join(metadata.template);
        let content = read_to_string(template_path).map_err(|err| {
            metrics::counter!("template_provider_error", "reason" => "template_not_found")
                .increment(1);
            tracing::debug!("template provider error: template not found ({:?})", err);
            Error::TemplateOpenFailed(err)
        })?;
        Ok(Template {
            name: metadata.name,
            description: metadata.description,
            content,
            attributes: metadata.attributes,
        })
    }
}
