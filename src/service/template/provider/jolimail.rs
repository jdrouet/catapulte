use super::TemplateProviderError;
use crate::service::template::manager::{TemplateManager, TemplateManagerError};
use crate::service::template::template::Template;
use async_trait::async_trait;

pub const CONFIG_BASE_URL: &str = "TEMPLATE_PROVIDER_JOLIMAIL_BASE_URL";

#[derive(Clone, Debug)]
pub struct JolimailTemplateProvider {
    base_url: String,
}

impl JolimailTemplateProvider {
    fn get_client() -> reqwest::Client {
        reqwest::Client::builder().use_rustls_tls().build().unwrap()
    }

    fn get_base_url_from_env() -> Result<String, TemplateProviderError> {
        match std::env::var(CONFIG_BASE_URL) {
            Ok(value) => Ok(value),
            Err(_) => Err(TemplateProviderError::ConfigurationInvalid(format!(
                "variable {} not found",
                CONFIG_BASE_URL
            ))),
        }
    }

    pub fn from_env() -> Result<Self, TemplateProviderError> {
        Ok(Self::new(Self::get_base_url_from_env()?))
    }

    pub fn new(base_url: String) -> Self {
        Self { base_url }
    }
}

#[async_trait]
impl TemplateManager for JolimailTemplateProvider {
    async fn find_by_name(&self, name: &str) -> Result<Template, TemplateManagerError> {
        let url = format!("{}/api/templates/{}/content", self.base_url, name);
        let request = Self::get_client().get(url.as_str()).send().await?;
        match request.status() {
            reqwest::StatusCode::NOT_FOUND => Err(TemplateManagerError::TemplateNotFound),
            _ => Ok(request.json::<Template>().await?),
        }
    }
}

// LCOV_EXCL_START
#[cfg(test)]
mod tests {
    use super::*;
    use env_test_util::TempEnvVar;
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    #[serial]
    fn from_env_without_variable() {
        let _env_base_url = TempEnvVar::new(CONFIG_BASE_URL);
        let result = JolimailTemplateProvider::from_env();
        assert!(result.is_err());
    }

    #[test]
    #[serial]
    fn from_env_with_variable() {
        let _env_base_url = TempEnvVar::new(CONFIG_BASE_URL).with("http://127.0.0.1:1234");
        let result = JolimailTemplateProvider::from_env();
        assert!(result.is_ok());
    }

    #[actix_rt::test]
    #[serial]
    async fn jolimail_find_by_slug_success() {
        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/templates/nice-slug/content"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "name": "nice-slug",
                "description": "yolo",
                "content": "<mjml></mjml>",
                "attributes": {}
            })))
            .mount(&mock_server)
            .await;
        let manager = JolimailTemplateProvider::new(mock_server.uri());
        assert!(manager.find_by_name("nice-slug").await.is_ok());
    }

    #[actix_rt::test]
    #[serial]
    async fn jolimail_find_by_slug_not_found() {
        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/templates/nice-slug/content"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&mock_server)
            .await;
        let manager = JolimailTemplateProvider::new(mock_server.uri());
        assert!(manager.find_by_name("nice-slug").await.is_err());
    }
}
// LCOV_EXCL_END
