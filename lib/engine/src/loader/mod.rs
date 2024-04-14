use catapulte_prelude::{EmbeddedTemplateDefinition, MetadataWithTemplate};

use self::http::HttpLoader;

pub mod http;
pub mod local;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Local(#[from] local::Error),
    #[error(transparent)]
    Http(#[from] http::Error),
    #[error("Multiple errors occured: {0:?}")]
    Multiple(Vec<Error>),
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
    Combined {
        local: local::LocalLoader,
        http: HttpLoader,
    },
    Local(local::LocalLoader),
    Http(http::HttpLoader),
}

impl Loader {
    pub async fn find_by_name(
        &self,
        name: &str,
    ) -> Result<MetadataWithTemplate<EmbeddedTemplateDefinition>, Error> {
        match self {
            Loader::Combined { local, http } => match local.find_by_name(name).await {
                Ok(found) => Ok(found),
                Err(local_error) => match http.find_by_name(name).await {
                    Ok(found) => Ok(found),
                    Err(http_error) => {
                        Err(Error::Multiple(vec![local_error.into(), http_error.into()]))
                    }
                },
            },
            Loader::Local(inner) => inner.find_by_name(name).await.map_err(Error::Local),
            Loader::Http(inner) => inner.find_by_name(name).await.map_err(Error::Http),
        }
    }
}
