mod http;
mod local;

pub use http::{HttpLoader, HttpLoaderConfig};
pub use local::{LocalLoader, LocalLoaderConfig};

use catapulte_domain::error::TemplateLoadError;
use catapulte_domain::model::Template;
use catapulte_domain::prelude::TemplateLoader;

/// A loader that tries multiple sources in order
#[derive(Debug)]
pub struct MultiLoader {
    loaders: Vec<AnyLoader>,
}

impl MultiLoader {
    pub fn new() -> Self {
        Self {
            loaders: Vec::new(),
        }
    }

    pub fn with_local(mut self, loader: LocalLoader) -> Self {
        self.loaders.push(AnyLoader::Local(loader));
        self
    }

    pub fn with_http(mut self, loader: HttpLoader) -> Self {
        self.loaders.push(AnyLoader::Http(loader));
        self
    }
}

impl Default for MultiLoader {
    fn default() -> Self {
        Self::new()
    }
}

impl TemplateLoader for MultiLoader {
    async fn load(&self, name: &str) -> Result<Template, TemplateLoadError> {
        let mut last_error = None;
        for loader in &self.loaders {
            match loader.load(name).await {
                Ok(template) => return Ok(template),
                Err(err) => {
                    last_error = Some(err);
                }
            }
        }
        Err(last_error.unwrap_or_else(|| TemplateLoadError::NotFound {
            name: name.to_string(),
        }))
    }
}

#[derive(Debug)]
enum AnyLoader {
    Local(LocalLoader),
    Http(HttpLoader),
}

impl AnyLoader {
    async fn load(&self, name: &str) -> Result<Template, TemplateLoadError> {
        match self {
            Self::Local(loader) => loader.load(name).await,
            Self::Http(loader) => loader.load(name).await,
        }
    }
}
