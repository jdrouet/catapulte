use super::manager::{TemplateManager, TemplateManagerError};
use super::template::Template;

pub mod local;

#[derive(Clone, Debug)]
pub enum TemplateProviderError {
    ConfigurationInvalid(String),
}

impl std::fmt::Display for TemplateProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "TemplateProviderError")
    }
}

#[derive(Clone, Debug)]
pub enum TemplateProvider {
    Local(local::LocalTemplateProvider),
}

impl TemplateProvider {
    pub fn from_env() -> Result<Self, TemplateProviderError> {
        let root = match std::env::var("TEMPLATE_ROOT") {
            Ok(value) => value,
            Err(_) => {
                return Err(TemplateProviderError::ConfigurationInvalid(
                    "variable TEMPLATE_ROOT not found".into(),
                ))
            }
        };
        Ok(TemplateProvider::local(root.as_str()))
    }

    fn local(root: &str) -> Self {
        TemplateProvider::Local(local::LocalTemplateProvider::new(root))
    }

    fn get_manager(&self) -> Box<&dyn TemplateManager> {
        match self {
            TemplateProvider::Local(manager) => Box::new(manager),
        }
    }
}

impl TemplateManager for TemplateProvider {
    fn find_by_name(&self, name: &str) -> Result<Template, TemplateManagerError> {
        self.get_manager().find_by_name(name)
    }
}
