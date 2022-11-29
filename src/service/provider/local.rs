use super::prelude::TemplateProviderError;
use crate::service::template::Template;
use serde::Deserialize;
use serde_json::Value as JsonValue;
use std::fs::{read_to_string, File};
use std::io::BufReader;
use std::path::PathBuf;

fn default_mjml_path() -> String {
    "template.mjml".into()
}

#[derive(Debug, Deserialize)]
pub struct LocalMetadata {
    name: String,
    description: String,
    #[serde(default = "default_mjml_path")]
    mjml: String,
    attributes: JsonValue,
}

#[derive(Clone, Debug)]
pub struct LocalTemplateProvider {
    root: PathBuf,
}

impl LocalTemplateProvider {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

impl LocalTemplateProvider {
    pub(super) async fn find_by_name(&self, name: &str) -> Result<Template, TemplateProviderError> {
        tracing::debug!("loading template {}", name);
        let path = self.root.join(name).join("metadata.json");
        let metadata_file = File::open(path)?;
        let metadata_reader = BufReader::new(metadata_file);
        let metadata: LocalMetadata = serde_json::from_reader(metadata_reader)?;
        let mjml_path = self.root.join(name).join(metadata.mjml);
        let content = read_to_string(mjml_path)?;
        Ok(Template {
            name: metadata.name,
            description: metadata.description,
            content,
            attributes: metadata.attributes,
        })
    }
}
