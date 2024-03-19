use std::{collections::HashMap, path::PathBuf};

#[derive(Clone, Debug, serde::Deserialize)]
pub struct Config {
    #[serde(default)]
    pub include_loader: Vec<IncludeLoaderEntry>,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(tag = "type")]
pub enum IncludeLoaderFilter {
    StartsWith { value: String },
    Any,
}

impl Default for IncludeLoaderFilter {
    fn default() -> Self {
        IncludeLoaderFilter::Any
    }
}

impl From<IncludeLoaderFilter> for mrml::prelude::parser::multi_loader::MultiIncludeLoaderFilter {
    fn from(value: IncludeLoaderFilter) -> Self {
        match value {
            IncludeLoaderFilter::Any => {
                mrml::prelude::parser::multi_loader::MultiIncludeLoaderFilter::Any
            }
            IncludeLoaderFilter::StartsWith { value } => {
                mrml::prelude::parser::multi_loader::MultiIncludeLoaderFilter::StartsWith { value }
            }
        }
    }
}

#[derive(Clone, Debug, serde::Deserialize)]
pub struct IncludeLoaderEntry {
    pub filter: IncludeLoaderFilter,
    pub loader: IncludeLoaderConfig,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(tag = "type")]
pub enum IncludeLoaderConfig {
    Local { root: String },
    Memory { values: HashMap<String, String> },
}

impl IncludeLoaderConfig {
    fn into_async_loader(
        self,
    ) -> Box<dyn mrml::prelude::parser::loader::AsyncIncludeLoader + Send + Sync> {
        match self {
            Self::Local { root } => Box::new(
                mrml::prelude::parser::local_loader::LocalIncludeLoader::new(PathBuf::from(root)),
            ),
            Self::Memory { values } => {
                Box::new(mrml::prelude::parser::memory_loader::MemoryIncludeLoader(
                    mrml::prelude::hash::Map::from_iter(values.into_iter()),
                ))
            }
        }
    }
}

impl From<Config> for mrml::prelude::parser::AsyncParserOptions {
    fn from(value: Config) -> Self {
        Self {
            include_loader: Box::new(value.include_loader.into_iter().fold(
                mrml::prelude::parser::multi_loader::MultiIncludeLoader::<
                    Box<
                        dyn mrml::prelude::parser::loader::AsyncIncludeLoader
                            + Send
                            + Sync
                            + 'static,
                    >,
                >::new(),
                |loader, item| match item.filter {
                    IncludeLoaderFilter::Any => loader.with_any(item.loader.into_async_loader()),
                    IncludeLoaderFilter::StartsWith { value } => {
                        loader.with_starts_with(value, item.loader.into_async_loader())
                    }
                },
            )),
        }
    }
}
