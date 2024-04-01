use catapulte_prelude::{EmbeddedTemplateDefinition, MetadataWithTemplate};

pub mod http;
pub mod local;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Local(#[from] local::Error),
    #[error(transparent)]
    Http(#[from] http::Error),
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Config {
    Local(local::Config),
    Http(http::Config),
}

impl Default for Config {
    fn default() -> Self {
        Self::Local(local::Config::default())
    }
}

impl From<Config> for Loader {
    fn from(value: Config) -> Self {
        match value {
            Config::Local(item) => Loader::Local(item.into()),
            Config::Http(item) => Loader::Http(item.build()),
        }
    }
}

#[derive(Debug)]
pub enum Loader {
    Local(local::LocalLoader),
    Http(http::HttpLoader),
}

impl Loader {
    pub async fn find_by_name(
        &self,
        name: &str,
    ) -> Result<MetadataWithTemplate<EmbeddedTemplateDefinition>, Error> {
        match self {
            Loader::Local(inner) => inner.find_by_name(name).await.map_err(Error::Local),
            Loader::Http(inner) => inner.find_by_name(name).await.map_err(Error::Http),
        }
    }
}
