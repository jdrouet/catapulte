pub mod local;
pub mod prelude;

use crate::service::template::Template;
use prelude::Error;

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub(crate) enum Configuration {
    Local(local::Configuration),
}

impl Default for Configuration {
    fn default() -> Self {
        Self::Local(local::Configuration::default())
    }
}

impl Configuration {
    pub(crate) fn build(&self) -> TemplateProvider {
        tracing::debug!("building template provider");
        match self {
            Self::Local(item) => TemplateProvider::Local(item.build()),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) enum TemplateProvider {
    Local(local::TemplateProvider),
}

impl TemplateProvider {
    pub async fn find_by_name(&self, name: &str) -> Result<Template, Error> {
        match self {
            Self::Local(inner) => inner.find_by_name(name).await,
        }
    }
}
