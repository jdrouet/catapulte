use super::prelude::{TemplateProvider, TemplateProviderError};
use crate::config::Config;
use crate::service::template::Template;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value as JsonValue;
use std::fs::{read_to_string, File};
use std::io::BufReader;
use std::path::Path;
use std::sync::Arc;

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
    root: String,
}

impl From<Arc<Config>> for LocalTemplateProvider {
    fn from(root: Arc<Config>) -> Self {
        Self {
            root: root.local_provider_root.clone(),
        }
    }
}

#[async_trait]
impl TemplateProvider for LocalTemplateProvider {
    async fn find_by_name(&self, name: &str) -> Result<Template, TemplateProviderError> {
        info!("find_by_name: {}", name);
        let path = Path::new(self.root.as_str())
            .join(name)
            .join("metadata.json");
        let metadata_file = File::open(path)?;
        let metadata_reader = BufReader::new(metadata_file);
        let metadata: LocalMetadata = serde_json::from_reader(metadata_reader)?;
        let mjml_path = Path::new(self.root.as_str()).join(name).join(metadata.mjml);
        let content = read_to_string(mjml_path)?;
        Ok(Template {
            name: metadata.name,
            description: metadata.description,
            content,
            attributes: metadata.attributes,
        })
    }
}

// LCOV_EXCL_START
#[cfg(test)]
mod tests {
    use super::LocalTemplateProvider;
    use super::TemplateProvider;
    use super::TemplateProviderError;
    use crate::config::Config;

    #[test]
    fn without_template_root() {
        let cfg = Config::from_args(vec![]);
        let result = LocalTemplateProvider::from(cfg);
        assert_eq!(result.root, "./template");
    }

    #[test]
    fn with_template_root() {
        let cfg = Config::from_args(vec![
            "--local-provider-root".to_string(),
            "./somewhere".to_string(),
        ]);
        let result = LocalTemplateProvider::from(cfg);
        assert_eq!(result.root, "./somewhere");
    }

    #[actix_rt::test]
    async fn local_find_by_name_not_found() {
        let cfg = Config::build();
        let manager = LocalTemplateProvider::from(cfg);
        assert!(match manager.find_by_name("not_found").await.unwrap_err() {
            TemplateProviderError::TemplateNotFound => true,
            _ => false,
        });
    }

    #[actix_rt::test]
    async fn local_find_by_name_success() {
        let cfg = Config::build();
        let manager = LocalTemplateProvider::from(cfg);
        let result = manager.find_by_name("user-login").await.unwrap();
        assert_eq!(result.name, "user-login");
        assert_eq!(result.description, "Template for login with magic link");
    }
}
// LCOV_EXCL_END
