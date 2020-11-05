use super::manager::{TemplateManager, TemplateManagerError};
use super::template::Template;

#[cfg(feature = "provider-jolimail")]
pub mod jolimail;
pub mod local;

const CONFIG_TEMPLATE_PROVIDER: &str = "TEMPLATE_PROVIDER";

#[derive(Clone, Debug)]
pub enum TemplateProviderError {
    ConfigurationInvalid(String),
}

#[derive(Clone, Debug)]
pub enum TemplateProvider {
    #[cfg(feature = "provider-jolimail")]
    Jolimail(jolimail::JolimailTemplateProvider),
    Local(local::LocalTemplateProvider),
}

impl TemplateProvider {
    pub fn from_env() -> Result<Self, TemplateProviderError> {
        match std::env::var(CONFIG_TEMPLATE_PROVIDER)
            .unwrap_or_else(|_| "local".into())
            .as_str()
        {
            #[cfg(feature = "provider-jolimail")]
            "jolimail" => Ok(Self::Jolimail(
                jolimail::JolimailTemplateProvider::from_env()?,
            )),
            _ => Ok(Self::Local(local::LocalTemplateProvider::from_env()?)),
        }
    }

    fn get_manager(&self) -> &dyn TemplateManager {
        match self {
            #[cfg(feature = "provider-jolimail")]
            Self::Jolimail(manager) => manager,
            Self::Local(manager) => manager,
        }
    }

    pub async fn find_by_name(&self, name: &str) -> Result<Template, TemplateManagerError> {
        self.get_manager().find_by_name(name).await
    }
}

// LCOV_EXCL_START
#[cfg(test)]
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
mod tests {
    use super::*;
    use env_test_util::TempEnvVar;

    #[test]
    #[serial]
    fn template_provider_from_env_local() {
        let _env_provider = TempEnvVar::new(CONFIG_TEMPLATE_PROVIDER).with("local");
        let _env_root = TempEnvVar::new(local::CONFIG_PROVIDER_LOCAL_ROOT).with("./template");
        let provider = TemplateProvider::from_env();
        assert!(provider.is_ok());
        assert!(provider.unwrap().is_local());
    }

    #[cfg(feature = "provider-jolimail")]
    #[test]
    #[serial]
    fn template_provider_from_env_jolimail() {
        let _env_provider = TempEnvVar::new(CONFIG_TEMPLATE_PROVIDER).with("jolimail");
        let _env_base_url = TempEnvVar::new(jolimail::CONFIG_BASE_URL).with("http://localhost");
        let provider = TemplateProvider::from_env();
        assert!(provider.is_ok());
        assert!(provider.unwrap().is_jolimail());
    }
}
// LCOV_EXCL_END
