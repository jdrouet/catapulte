use std::collections::BTreeMap;

use catapulte_domain::error::TemplateLoadError;
use catapulte_domain::model::{Template, TemplateMetadata};
use catapulte_domain::prelude::TemplateLoader;
use reqwest::Url;
use reqwest::header::HeaderMap;

/// Configuration for HTTP template loading
#[derive(Clone, Debug, serde::Deserialize)]
pub struct HttpLoaderConfig {
    pub url: String,
    #[serde(default)]
    pub params: BTreeMap<String, String>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
}

/// Loads templates from an HTTP endpoint
#[derive(Debug, Clone)]
pub struct HttpLoader {
    client: reqwest::Client,
    url: String,
    params: Vec<(String, String)>,
    headers: HeaderMap,
}

impl HttpLoader {
    pub fn new(config: &HttpLoaderConfig) -> Self {
        Self {
            client: reqwest::Client::new(),
            url: config.url.clone(),
            params: config.params.clone().into_iter().collect(),
            headers: config
                .headers
                .iter()
                .filter_map(|(name, value)| {
                    let name = reqwest::header::HeaderName::from_bytes(name.as_bytes()).ok()?;
                    let value = reqwest::header::HeaderValue::from_bytes(value.as_bytes()).ok()?;
                    Some((name, value))
                })
                .collect(),
        }
    }

    #[cfg(test)]
    fn from_url(url: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            url,
            params: Vec::new(),
            headers: HeaderMap::new(),
        }
    }

    fn interpolate(&self, name: &str, filename: &str) -> String {
        if self.url.ends_with('/') {
            format!("{}{}/{}", self.url, name, filename)
        } else {
            format!("{}/{}/{}", self.url, name, filename)
        }
    }

    fn build_url(&self, name: &str, filename: &str) -> Result<Url, TemplateLoadError> {
        let base_url = self.interpolate(name, filename);
        Url::parse_with_params(&base_url, self.params.iter()).map_err(|err| {
            tracing::error!("unable to generate metadata url: {:?}", err);
            TemplateLoadError::FetchError(anyhow::Error::new(err))
        })
    }

    async fn query_url(&self, url: Url) -> Result<reqwest::Response, TemplateLoadError> {
        self.client
            .get(url)
            .headers(self.headers.clone())
            .send()
            .await
            .map_err(|err| {
                tracing::error!("unable to execute request: {:?}", err);
                TemplateLoadError::FetchError(anyhow::Error::new(err))
            })
    }
}

/// Internal metadata format for HTTP templates
#[derive(Debug, serde::Deserialize)]
struct MetadataResponse {
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    attributes: Option<serde_json::Value>,
    template: TemplateDefinition,
}

#[derive(Debug, serde::Deserialize)]
#[serde(untagged)]
enum TemplateDefinition {
    Embedded { content: String },
    Remote { url: Url },
}

impl TemplateLoader for HttpLoader {
    async fn load(&self, name: &str) -> Result<Template, TemplateLoadError> {
        tracing::debug!("loading template {} from HTTP", name);

        let url = self.build_url(name, "metadata.json")?;
        let response = self.query_url(url).await?;
        let response = response.error_for_status().map_err(|err| {
            if err.status() == Some(reqwest::StatusCode::NOT_FOUND) {
                TemplateLoadError::NotFound {
                    name: name.to_string(),
                }
            } else {
                TemplateLoadError::FetchError(anyhow::Error::new(err))
            }
        })?;

        let metadata: MetadataResponse = response.json().await.map_err(|err| {
            tracing::error!("unable to parse metadata: {:?}", err);
            TemplateLoadError::InvalidMetadata(anyhow::Error::new(err))
        })?;

        let content = match metadata.template {
            TemplateDefinition::Embedded { content } => content,
            TemplateDefinition::Remote { url } => {
                let response = self.query_url(url).await?;
                let response = response
                    .error_for_status()
                    .map_err(|err| TemplateLoadError::FetchError(anyhow::Error::new(err)))?;
                response.text().await.map_err(|err| {
                    tracing::error!("unable to load template content: {:?}", err);
                    TemplateLoadError::FetchError(anyhow::Error::new(err))
                })?
            }
        };

        Ok(Template {
            metadata: TemplateMetadata {
                name: metadata.name,
                description: metadata.description,
                attributes: metadata.attributes,
            },
            content,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn should_fail_when_not_found() {
        let mock_server = MockServer::start().await;
        let loader = HttpLoader::from_url(format!("{}/templates/", mock_server.uri()));

        let err = loader.load("user-login").await.unwrap_err();
        assert!(matches!(err, TemplateLoadError::NotFound { .. }));
    }

    #[tokio::test]
    async fn should_load_embedded_template() {
        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/templates/user-login/metadata.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "name": "user-login",
                "description": "Test template",
                "template": {
                    "content": "<mjml><mj-body></mj-body></mjml>"
                }
            })))
            .mount(&mock_server)
            .await;

        let loader = HttpLoader::from_url(format!("{}/templates/", mock_server.uri()));
        let template = loader.load("user-login").await.unwrap();

        assert_eq!(template.metadata.name, "user-login");
        assert_eq!(
            template.metadata.description.as_deref(),
            Some("Test template")
        );
        assert!(template.content.contains("<mjml>"));
    }

    #[tokio::test]
    async fn should_load_remote_template() {
        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/templates/user-login/metadata.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "name": "user-login",
                "template": {
                    "url": format!("{}/templates/user-login/template.mjml", mock_server.uri())
                }
            })))
            .mount(&mock_server)
            .await;
        Mock::given(method("GET"))
            .and(path("/templates/user-login/template.mjml"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string("<mjml><mj-body></mj-body></mjml>"),
            )
            .mount(&mock_server)
            .await;

        let loader = HttpLoader::from_url(format!("{}/templates/", mock_server.uri()));
        let template = loader.load("user-login").await.unwrap();

        assert_eq!(template.metadata.name, "user-login");
        assert!(template.content.contains("<mjml>"));
    }
}
