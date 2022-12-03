use super::prelude::Error;
use crate::service::template::Template;
use serde::Deserialize;
use serde_json::Value as JsonValue;
use std::borrow::Cow;
use std::fs::{read_to_string, File};
use std::io::BufReader;
use std::path::PathBuf;

#[derive(Clone, Debug, serde::Deserialize)]
pub(crate) struct Configuration {
    path: PathBuf,
}

impl Default for Configuration {
    fn default() -> Self {
        Self {
            path: PathBuf::new().join("template"),
        }
    }
}

impl Configuration {
    pub(crate) fn build(&self) -> TemplateProvider {
        tracing::debug!("building template provider");
        TemplateProvider::new(self.path.clone())
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

#[derive(Clone, Debug)]
pub struct TemplateProvider {
    root: PathBuf,
}

impl TemplateProvider {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

impl TemplateProvider {
    pub(super) async fn find_by_name(&self, name: &str) -> Result<Template, Error> {
        tracing::debug!("loading template {}", name);
        let path = self.root.join(name).join("metadata.json");
        let metadata_file = File::open(path).map_err(|err| {
            metrics::increment_counter!("template_provider_error", "reason" => "metadata_not_found");
            tracing::debug!("template provider error: metadata not found ({:?})", err);
            Error::Loading {
                origin: Cow::Borrowed("opening metadata"),
            }
        })?;
        let metadata_reader = BufReader::new(metadata_file);
        let metadata: LocalMetadata = serde_json::from_reader(metadata_reader).map_err(|err| {
            metrics::increment_counter!("template_provider_error", "reason" => "metadata_invalid");
            tracing::debug!("template provider error: metadata invalid ({:?})", err);
            Error::Loading {
                origin: Cow::Borrowed("parsing metadata"),
            }
        })?;
        let template_path = self.root.join(name).join(metadata.template);
        let content = read_to_string(template_path).map_err(|err| {
            metrics::increment_counter!("template_provider_error", "reason" => "template_not_found");
            tracing::debug!("template provider error: template not found ({:?})", err);
            Error::Loading {
                origin: Cow::Borrowed("reading template"),
            }
        })?;
        Ok(Template {
            name: metadata.name,
            description: metadata.description,
            content,
            attributes: metadata.attributes,
        })
    }
}
