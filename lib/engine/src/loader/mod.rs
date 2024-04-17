use catapulte_prelude::{EmbeddedTemplateDefinition, MetadataWithTemplate};

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

#[derive(Clone, Debug, Default, serde::Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub struct Config {
    pub local: local::Config,
    pub http: Option<http::Config>,
}

impl From<Config> for Loader {
    fn from(value: Config) -> Self {
        let mut loaders = Vec::with_capacity(2);
        loaders.push(AnyLoader::Local(value.local.into()));
        if let Some(http) = value.http {
            loaders.push(AnyLoader::Http(http.into()));
        }
        Self { loaders }
    }
}

#[derive(Debug)]
pub struct Loader {
    loaders: Vec<AnyLoader>,
}

impl Loader {
    pub async fn find_by_name(
        &self,
        name: &str,
    ) -> Result<MetadataWithTemplate<EmbeddedTemplateDefinition>, Error> {
        let mut errors = Vec::with_capacity(self.loaders.len());
        for loader in self.loaders.iter() {
            match loader.find_by_name(name).await {
                Ok(found) => return Ok(found),
                Err(err) => {
                    errors.push(err);
                }
            }
        }
        Err(Error::Multiple(errors))
    }
}

#[derive(Debug)]
pub enum AnyLoader {
    Local(local::LocalLoader),
    Http(http::HttpLoader),
}

impl AnyLoader {
    pub async fn find_by_name(
        &self,
        name: &str,
    ) -> Result<MetadataWithTemplate<EmbeddedTemplateDefinition>, Error> {
        match self {
            Self::Local(inner) => inner.find_by_name(name).await.map_err(Error::Local),
            Self::Http(inner) => inner.find_by_name(name).await.map_err(Error::Http),
        }
    }
}
