use std::fs::{File, read_to_string};
use std::io::BufReader;
use std::path::PathBuf;

use catapulte_domain::error::TemplateLoadError;
use catapulte_domain::model::{Template, TemplateMetadata};
use catapulte_domain::prelude::TemplateLoader;

/// Configuration for local template loading
#[derive(Clone, Debug, serde::Deserialize)]
pub struct LocalLoaderConfig {
    pub path: PathBuf,
}

impl Default for LocalLoaderConfig {
    fn default() -> Self {
        Self {
            path: PathBuf::from("template"),
        }
    }
}

/// Loads templates from the local filesystem
#[derive(Debug)]
pub struct LocalLoader {
    root: PathBuf,
}

impl LocalLoader {
    pub fn new(config: &LocalLoaderConfig) -> Self {
        Self {
            root: config.path.clone(),
        }
    }

    pub fn from_path(path: impl Into<PathBuf>) -> Self {
        Self { root: path.into() }
    }
}

/// Internal metadata format for local templates
#[derive(Debug, serde::Deserialize)]
struct MetadataFile {
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    attributes: Option<serde_json::Value>,
    template: TemplateDefinition,
}

#[derive(Debug, serde::Deserialize)]
#[serde(untagged)]
enum TemplateDefinition {
    Embedded { content: String },
    Local { path: PathBuf },
}

impl Default for TemplateDefinition {
    fn default() -> Self {
        Self::Local {
            path: PathBuf::from("template.mjml"),
        }
    }
}

impl TemplateLoader for LocalLoader {
    async fn load(&self, name: &str) -> Result<Template, TemplateLoadError> {
        tracing::debug!("loading template {}", name);

        let metadata_path = self.root.join(name).join("metadata.json");
        let metadata_file = File::open(&metadata_path).map_err(|err| {
            metrics::counter!("template_provider_error", "reason" => "metadata_not_found")
                .increment(1);
            tracing::debug!("template provider error: metadata not found ({:?})", err);
            TemplateLoadError::NotFound {
                name: name.to_string(),
            }
        })?;

        let metadata_reader = BufReader::new(metadata_file);
        let metadata: MetadataFile = serde_json::from_reader(metadata_reader).map_err(|err| {
            metrics::counter!("template_provider_error", "reason" => "metadata_invalid")
                .increment(1);
            tracing::debug!("template provider error: metadata invalid ({:?})", err);
            TemplateLoadError::InvalidMetadata(anyhow::Error::new(err))
        })?;

        let content = match metadata.template {
            TemplateDefinition::Embedded { content } => content,
            TemplateDefinition::Local { path } => {
                let template_path = self.root.join(name).join(path);
                read_to_string(&template_path).map_err(|err| {
                    metrics::counter!("template_provider_error", "reason" => "template_not_found")
                        .increment(1);
                    tracing::debug!("template provider error: template not found ({:?})", err);
                    TemplateLoadError::IoError(anyhow::Error::new(err))
                })?
            }
        };

        Ok(Template {
            metadata: TemplateMetadata {
                name: metadata.name,
                description: metadata.description,
                attributes: metadata.attributes,
            },
            content,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_loader() -> LocalLoader {
        LocalLoader::from_path(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join("..")
                .join("template"),
        )
    }

    #[tokio::test]
    async fn should_find_template() {
        let loader = test_loader();
        let template = loader.load("user-login").await.unwrap();
        assert_eq!(template.metadata.name, "user-login");
        assert_eq!(
            template.metadata.description.as_deref(),
            Some("Template for login with magic link")
        );
        assert!(template.content.contains("<mjml>"));
    }

    #[tokio::test]
    async fn should_fail_when_metadata_not_found() {
        let loader = test_loader();
        let err = loader.load("not-found").await.unwrap_err();
        assert!(matches!(err, TemplateLoadError::NotFound { .. }));
    }

    #[tokio::test]
    async fn should_fail_when_invalid_metadata() {
        let loader = test_loader();
        let err = loader.load("invalid-metadata").await.unwrap_err();
        assert!(matches!(err, TemplateLoadError::InvalidMetadata(_)));
    }

    #[tokio::test]
    async fn should_load_embedded_template() {
        let loader = test_loader();
        let template = loader.load("embedded").await.unwrap();
        assert_eq!(template.metadata.name, "embedded");
        assert_eq!(template.content, "<mjml></mjml>");
    }

    #[tokio::test]
    async fn should_fail_when_template_file_not_found() {
        let loader = test_loader();
        let err = loader.load("template-not-found").await.unwrap_err();
        assert!(matches!(err, TemplateLoadError::IoError(_)));
    }
}
