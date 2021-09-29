use crate::config::Config;
use crate::service::template::manager::{TemplateManager, TemplateManagerError};
use crate::service::template::template::Template;
use async_trait::async_trait;
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct JolimailTemplateProvider {
    base_url: String,
}

impl From<Arc<Config>> for JolimailTemplateProvider {
    fn from(root: Arc<Config>) -> Self {
        let base_url = root
            .jolimail_provider_url
            .clone()
            .expect("no jolimail url found");
        Self { base_url }
    }
}

impl JolimailTemplateProvider {
    #[cfg(test)]
    fn new(base_url: String) -> Self {
        Self { base_url }
    }

    fn get_client() -> reqwest::Client {
        reqwest::Client::builder().use_rustls_tls().build().unwrap()
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
    use super::JolimailTemplateProvider;
    use super::TemplateManager;
    use crate::config::Config;
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    #[should_panic]
    fn from_env_without_variable() {
        let cfg = Config::from_args(vec![]);
        let _ = JolimailTemplateProvider::from(cfg);
    }

    #[test]
    fn from_env_with_variable() {
        let cfg = Config::from_args(vec![
            "--jolimail-provider-url".to_string(),
            "http://127.0.0.1:1234".to_string(),
        ]);
        let _ = JolimailTemplateProvider::from(cfg);
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
