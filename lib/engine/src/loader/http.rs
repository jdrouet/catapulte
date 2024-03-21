use super::prelude::Template;
use reqwest::{header::HeaderMap, Url};
use serde::Deserialize;
use serde_json::Value as JsonValue;
use std::collections::BTreeMap;

#[derive(Clone, Debug, serde::Deserialize)]
pub struct Config {
    pub url: String,
    pub params: BTreeMap<String, String>,
    pub headers: BTreeMap<String, String>,
}

impl Config {
    pub fn build(&self) -> HttpLoader {
        tracing::debug!("building template provider");
        HttpLoader {
            client: reqwest::Client::new(),
            url: self.url.clone(),
            params: self
                .params
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
            headers: self
                .headers
                .iter()
                .map(|(name, value)| {
                    (
                        reqwest::header::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                        reqwest::header::HeaderValue::from_bytes(value.as_bytes()).unwrap(),
                    )
                })
                .collect(),
        }
    }
}

impl From<Config> for HttpLoader {
    fn from(value: Config) -> Self {
        HttpLoader {
            client: reqwest::Client::new(),
            url: value.url,
            params: value.params.into_iter().collect(),
            headers: value
                .headers
                .into_iter()
                .map(|(name, value)| {
                    (
                        reqwest::header::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                        reqwest::header::HeaderValue::from_bytes(value.as_bytes()).unwrap(),
                    )
                })
                .collect(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Unable to load and parse metadata: {0:?}")]
    MetadataLoadingFailed(reqwest::Error),
    #[error("Unable to build metadata url: {0:?}")]
    MetadataUrlInvalid(url::ParseError),
    #[error("Unable to request file: {0:?}")]
    RequestFailed(reqwest::Error),
    #[error("Unable to load and parse template: {0:?}")]
    TemplateLoadingFailed(reqwest::Error),
}

#[derive(Debug, Deserialize)]
struct RemoteMetadata {
    name: String,
    description: String,
    #[serde(flatten)]
    template: RemoteMetadataTemplate,
    attributes: JsonValue,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RemoteMetadataTemplate {
    Embedded { content: String },
    Remote { template: String },
}

#[derive(Clone, Debug)]
pub struct HttpLoader {
    client: reqwest::Client,
    url: String,
    params: Vec<(String, String)>,
    headers: HeaderMap,
}

impl HttpLoader {
    #[cfg(test)]
    fn new(url: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            url,
            params: Default::default(),
            headers: Default::default(),
        }
    }

    fn interpolate(&self, name: &str, filename: &str) -> String {
        if self.url.ends_with('/') {
            format!("{}{}/{}", self.url, name, filename)
        } else {
            format!("{}/{}/{}", self.url, name, filename)
        }
    }

    fn build_url(&self, name: &str, filename: &str) -> Result<Url, Error> {
        let base_url = self.interpolate(name, filename);
        Url::parse_with_params(base_url.as_str(), self.params.iter()).map_err(|err| {
            tracing::error!("unable to generate metadata url: {:?}", err);
            Error::MetadataUrlInvalid(err)
        })
    }

    async fn build_request(&self, name: &str, filename: &str) -> Result<reqwest::Response, Error> {
        let url = self.build_url(name, filename)?;
        self.client
            .get(url)
            .headers(self.headers.clone())
            .send()
            .await
            .map_err(|err| {
                tracing::error!("unable to request template {}: {:?}", filename, err);
                Error::RequestFailed(err)
            })
    }

    pub(super) async fn find_by_name(&self, name: &str) -> Result<Template, Error> {
        tracing::debug!("loading template {}", name);
        let res = self.build_request(name, "metadata.json").await?;
        let res = res
            .error_for_status()
            .map_err(Error::MetadataLoadingFailed)?;
        let metadata: RemoteMetadata = res.json().await.map_err(|err| {
            tracing::error!("unable to load and parse metadata: {:?}", err);
            Error::MetadataLoadingFailed(err)
        })?;
        let content = match metadata.template {
            RemoteMetadataTemplate::Embedded { content } => content,
            RemoteMetadataTemplate::Remote { template } => {
                let res = self.build_request(name, &template).await?;
                let res = res
                    .error_for_status()
                    .map_err(Error::TemplateLoadingFailed)?;
                res.text().await.map_err(|err| {
                    tracing::error!("unable to load template content: {:?}", err);
                    Error::TemplateLoadingFailed(err)
                })?
            }
        };
        Ok(Template {
            name: metadata.name,
            description: metadata.description,
            content,
            attributes: metadata.attributes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::HttpLoader;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn interpolate(url: &str, name: &str) -> String {
        HttpLoader::new(url.into()).interpolate(name, "metadata.json")
    }

    #[test]
    fn should_interpolate_template_name() {
        assert_eq!(
            interpolate(
                "https://raw.githubusercontent.com/jdrouet/catapulte/main/template/",
                "user-login"
            ),
            "https://raw.githubusercontent.com/jdrouet/catapulte/main/template/user-login/metadata.json"
        );
    }

    #[tokio::test]
    async fn fetch_not_found_template() {
        let mock_server = MockServer::start().await;

        let provider = HttpLoader::new(format!("{}/templates/", &mock_server.uri()));
        let result = provider.find_by_name("user-login").await.unwrap_err();
        assert!(matches!(result, super::Error::MetadataLoadingFailed(_)));
    }

    #[tokio::test]
    async fn fetch_template_separate_file() {
        let metadata: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../template/user-login/metadata.json"
        ))
        .unwrap();
        let content = include_str!("../../../../template/user-login/template.mjml");
        //
        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/templates/user-login/metadata.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(metadata))
            .mount(&mock_server)
            .await;
        Mock::given(method("GET"))
            .and(path("/templates/user-login/template.mjml"))
            .respond_with(ResponseTemplate::new(200).set_body_string(content))
            .mount(&mock_server)
            .await;

        let provider = HttpLoader::new(format!("{}/templates/", &mock_server.uri()));
        let result = provider.find_by_name("user-login").await.unwrap();
        assert!(result.content.starts_with("<mjml>"));
    }

    #[tokio::test]
    async fn fetch_template_separate_file_missing_template() {
        let metadata: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../template/user-login/metadata.json"
        ))
        .unwrap();
        //
        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/templates/user-login/metadata.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(metadata))
            .mount(&mock_server)
            .await;

        let provider = HttpLoader::new(format!("{}/templates/", &mock_server.uri()));
        let result = provider.find_by_name("user-login").await.unwrap_err();
        assert!(matches!(result, super::Error::TemplateLoadingFailed(_)));
    }

    #[tokio::test]
    async fn fetch_template_same_file() {
        let content = include_str!("../../../../template/user-login/template.mjml");
        let metadata = serde_json::json!({
            "name": "single-file",
            "description": "read from single file",
            "content": content,
            "attributes": serde_json::json!({
                "type": "object",
                "properties": serde_json::json!({
                    "token": serde_json::json!({
                        "type": "string"
                    })
                }),
                "required": serde_json::json!([
                    "token"
                ])
            })
        });
        //
        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/templates/single-file/metadata.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(metadata))
            .mount(&mock_server)
            .await;
        let provider = HttpLoader::new(format!("{}/templates/", &mock_server.uri()));
        let result = provider.find_by_name("single-file").await.unwrap();
        assert!(result.content.starts_with("<mjml>"));
    }
}
