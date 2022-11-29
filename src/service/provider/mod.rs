pub mod local;
pub mod prelude;

use crate::service::template::Template;
use prelude::TemplateProviderError;
use std::path::PathBuf;

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(tag = "type")]
pub(crate) enum Configuration {
    Local { path: PathBuf },
}

impl Default for Configuration {
    fn default() -> Self {
        Self::Local {
            path: PathBuf::new().join("template"),
        }
    }
}

impl Configuration {
    pub(crate) fn build(&self) -> TemplateProvider {
        tracing::debug!("building template provider");
        match self {
            Self::Local { path } => {
                TemplateProvider::Local(local::LocalTemplateProvider::new(path.clone()))
            }
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) enum TemplateProvider {
    Local(local::LocalTemplateProvider),
}

impl TemplateProvider {
    pub async fn find_by_name(&self, name: &str) -> Result<Template, TemplateProviderError> {
        match self {
            Self::Local(inner) => inner.find_by_name(name).await,
        }
    }
}
