#[cfg(feature = "provider-jolimail")]
pub mod jolimail;
pub mod local;
pub mod prelude;

use crate::config::Config;
use crate::service::template::Template;
use prelude::{TemplateProvider as TemplateManager, TemplateProviderError};
use std::sync::Arc;

#[derive(Clone, Debug)]
pub enum TemplateProvider {
    #[cfg(feature = "provider-jolimail")]
    Jolimail(jolimail::JolimailTemplateProvider),
    Local(local::LocalTemplateProvider),
}

impl From<Arc<Config>> for TemplateProvider {
    fn from(root: Arc<Config>) -> Self {
        match root.template_provider.as_str() {
            #[cfg(feature = "provider-jolimail")]
            "jolimail" => Self::Jolimail(jolimail::JolimailTemplateProvider::from(root)),
            "local" => Self::Local(local::LocalTemplateProvider::from(root)),
            other => panic!("unknown template provider {}", other),
        }
    }
}

impl TemplateProvider {
    fn inner(&self) -> &dyn TemplateManager {
        match self {
            #[cfg(feature = "provider-jolimail")]
            Self::Jolimail(manager) => manager,
            Self::Local(manager) => manager,
        }
    }

    pub async fn find_by_name(&self, name: &str) -> Result<Template, TemplateProviderError> {
        self.inner().find_by_name(name).await
    }
}

#[cfg(test)]
#[cfg_attr(tarpaulin, ignore)]
impl TemplateProvider {
    #[cfg(feature = "provider-jolimail")]
    fn is_jolimail(&self) -> bool {
        match self {
            Self::Jolimail(_) => true,
            _ => false,
        }
    }

    fn is_local(&self) -> bool {
        match self {
            Self::Local(_) => true,
            _ => false,
        }
    }
}

#[cfg(test)]
#[cfg_attr(tarpaulin, ignore)]
mod tests {
    use super::TemplateProvider;
    use crate::config::Config;

    #[test]
    fn template_provider_from_env_local() {
        let cfg = Config::from_args(vec![
            "--template-provider".to_string(),
            "local".to_string(),
            "--local-provider-root".to_string(),
            "./template".to_string(),
        ]);
        let provider = TemplateProvider::from(cfg);
        assert!(provider.is_local());
    }

    #[cfg(feature = "provider-jolimail")]
    #[test]
    fn template_provider_from_env_jolimail() {
        let cfg = Config::from_args(vec![
            "--template-provider".to_string(),
            "jolimail".to_string(),
            "--jolimail-provider-url".to_string(),
            "http://127.0.0.1".to_string(),
        ]);
        let provider = TemplateProvider::from(cfg);
        assert!(provider.is_jolimail());
    }
}
