use crate::service::template::manager::{TemplateManager, TemplateManagerError};
use crate::service::template::template::Template;
use serde::Deserialize;
use std::fs::{read_to_string, File};
use std::io::BufReader;
use std::path::Path;

fn default_mjml_path() -> String {
    "template.mjml".into()
}

#[derive(Debug, Deserialize)]
pub struct LocalMetadata {
    name: String,
    description: String,
    #[serde(default = "default_mjml_path")]
    mjml: String,
}

#[derive(Clone, Debug)]
pub struct LocalTemplateProvider {
    root: String,
}

impl LocalTemplateProvider {
    pub fn new(root: &str) -> Self {
        Self { root: root.into() }
    }
}

impl TemplateManager for LocalTemplateProvider {
    fn find_by_name(&self, name: &str) -> Result<Template, TemplateManagerError> {
        info!("find_by_name: {}", name);
        let path = Path::new(self.root.as_str())
            .join(name)
            .join("metadata.json");
        let metadata_file = File::open(path)?;
        let metadata_reader = BufReader::new(metadata_file);
        let metadata: LocalMetadata = serde_json::from_reader(metadata_reader)?;
        let mjml_path = Path::new(self.root.as_str()).join(name).join(metadata.mjml);
        let mjml = read_to_string(mjml_path)?;
        Ok(Template {
            name: metadata.name,
            description: metadata.description,
            mjml,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get_root() -> String {
        match std::env::var("TEMPLATE_ROOT") {
            Ok(value) => value,
            Err(_) => String::from("template"),
        }
    }

    #[test]
    fn local_find_by_name_not_found() {
        let manager = LocalTemplateProvider::new(get_root().as_str());
        assert_eq!(
            manager.find_by_name("not_found").unwrap_err(),
            TemplateManagerError::TemplateNotFound
        );
    }

    #[test]
    fn local_find_by_name_success() {
        let manager = LocalTemplateProvider::new(get_root().as_str());
        let result = manager.find_by_name("user-login").unwrap();
        assert_eq!(result.name, "user-login");
        assert_eq!(result.description, "Template for login with magic link");
    }
}
