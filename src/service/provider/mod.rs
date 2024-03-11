pub mod http;
pub mod local;
pub mod prelude;

use std::sync::Arc;

use crate::service::template::Template;
use prelude::Error;

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub(crate) enum Configuration {
    Local(local::Configuration),
    Http(http::Configuration),
}

impl Default for Configuration {
    fn default() -> Self {
        Self::Local(local::Configuration::default())
    }
}

impl Configuration {
    pub(crate) fn build(&self) -> TemplateProvider {
        tracing::debug!("building template provider");
        TemplateProvider(Arc::new(match self {
            Self::Local(item) => InnerTemplateProvider::Local(item.build()),
            Self::Http(item) => InnerTemplateProvider::Http(item.build()),
        }))
    }
}

enum InnerTemplateProvider {
    Local(local::TemplateProvider),
    Http(http::TemplateProvider),
}

#[derive(Clone)]
pub(crate) struct TemplateProvider(Arc<InnerTemplateProvider>);

impl TemplateProvider {
    pub async fn find_by_name(&self, name: &str) -> Result<Template, Error> {
        match self.0.as_ref() {
            InnerTemplateProvider::Local(inner) => inner.find_by_name(name).await,
            InnerTemplateProvider::Http(inner) => inner.find_by_name(name).await,
        }
    }
}
