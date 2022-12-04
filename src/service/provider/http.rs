use super::prelude::Error;
use crate::service::template::Template;
use axum::http::HeaderMap;
use reqwest::{StatusCode, Url};
use serde::Deserialize;
use serde_json::Value as JsonValue;
use std::borrow::Cow;
use std::collections::BTreeMap;

#[derive(Clone, Debug, serde::Deserialize)]
pub(crate) struct Configuration {
    pub url: String,
    pub params: BTreeMap<String, String>,
    pub headers: BTreeMap<String, String>,
}

impl Configuration {
    pub(crate) fn build(&self) -> TemplateProvider {
        tracing::debug!("building template provider");
        TemplateProvider {
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

#[derive(Debug, Deserialize)]
pub struct RemoteMetadata {
    name: String,
    description: String,
    #[serde(flatten)]
    template: RemoteMetadataTemplate,
    attributes: JsonValue,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum RemoteMetadataTemplate {
    Embedded { content: String },
    Remote { template: String },
}

impl RemoteMetadata {
    async fn content(&self, provider: &TemplateProvider, name: &str) -> Result<String, Error> {
        match &self.template {
            RemoteMetadataTemplate::Embedded { content } => Ok(content.to_string()),
            RemoteMetadataTemplate::Remote { template } => {
                let res = provider.build_request(name, template).await?;
                let status = res.status();
                if status.is_client_error() {
                    tracing::error!("unable to load template content: {}", status);
                    Err(Error::internal(
                        "http",
                        Cow::Borrowed("error when loading template"),
                    ))
                } else if status.is_server_error() {
                    tracing::error!("unable to load template content: {}", status);
                    Err(Error::provider(
                        "http",
                        Cow::Borrowed("error when loading template"),
                    ))
                } else {
                    res.text().await.map_err(|err| {
                        tracing::error!("unable to load template content: {:?}", err);
                        Error::provider("http", Cow::Borrowed("unable to load template content"))
                    })
                }
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct TemplateProvider {
    client: reqwest::Client,
    url: String,
    params: Vec<(String, String)>,
    headers: HeaderMap,
}

impl TemplateProvider {
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
            Error::configuration(
                "http",
                Cow::Owned(format!(
                    "unable to build url for template {name} and {filename}"
                )),
            )
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
                Error::configuration("http", Cow::Borrowed("unable to request template"))
            })
    }

    pub(super) async fn find_by_name(&self, name: &str) -> Result<Template, Error> {
        tracing::debug!("loading template {}", name);
        let res = self.build_request(name, "metadata.json").await?;
        let status = res.status();
        if status == StatusCode::NOT_FOUND {
            tracing::error!("unable to find template metadata: {}", status);
            return Err(Error::not_found(
                "http",
                Cow::Borrowed("error when loading metadata"),
            ));
        } else if status.is_client_error() {
            tracing::error!("unable to load template metadata: {}", status);
            return Err(Error::internal(
                "http",
                Cow::Borrowed("error when loading metadata"),
            ));
        } else if status.is_server_error() {
            tracing::error!("unable to load template metadata: {}", status);
            return Err(Error::provider(
                "http",
                Cow::Borrowed("error when loading metadata"),
            ));
        }
        let metadata: RemoteMetadata = res.json().await.map_err(|err| {
            tracing::error!("unable to parse template metadata: {:?}", err);
            Error::provider("http", Cow::Borrowed("unable to parse template metadata"))
        })?;
        let template_content = metadata.content(self, name).await.map_err(|err| {
            tracing::error!("unable to load template content: {:?}", err);
            Error::provider("http", Cow::Borrowed("unable to load template content"))
        })?;
        Ok(Template {
            name: metadata.name,
            description: metadata.description,
            content: template_content,
            attributes: metadata.attributes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::TemplateProvider;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn interpolate(url: &str, name: &str) -> String {
        TemplateProvider::new(url.into()).interpolate(name, "metadata.json")
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
        crate::try_init_logs();
        let mock_server = MockServer::start().await;

        let provider = TemplateProvider::new(format!("{}/templates/", &mock_server.uri()));
        let result = provider.find_by_name("user-login").await.unwrap_err();
        assert_eq!(result.provider, "http");
        assert_eq!(result.message, "error when loading metadata");
    }

    #[tokio::test]
    async fn fetch_template_separate_file() {
        crate::try_init_logs();
        //
        let metadata: serde_json::Value =
            serde_json::from_str(include_str!("../../../template/user-login/metadata.json"))
                .unwrap();
        let content = include_str!("../../../template/user-login/template.mjml");
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

        let provider = TemplateProvider::new(format!("{}/templates/", &mock_server.uri()));
        let result = provider.find_by_name("user-login").await.unwrap();
        assert!(result.content.starts_with("<mjml>"));
    }

    #[tokio::test]
    async fn fetch_template_separate_file_missing_template() {
        crate::try_init_logs();
        //
        let metadata: serde_json::Value =
            serde_json::from_str(include_str!("../../../template/user-login/metadata.json"))
                .unwrap();
        //
        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/templates/user-login/metadata.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(metadata))
            .mount(&mock_server)
            .await;

        let provider = TemplateProvider::new(format!("{}/templates/", &mock_server.uri()));
        let result = provider.find_by_name("user-login").await.unwrap_err();
        assert_eq!(result.provider, "http");
        assert_eq!(result.message, "unable to load template content");
    }

    #[tokio::test]
    async fn fetch_template_same_file() {
        crate::try_init_logs();
        //
        let content = include_str!("../../../template/user-login/template.mjml");
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
        let provider = TemplateProvider::new(format!("{}/templates/", &mock_server.uri()));
        let result = provider.find_by_name("single-file").await.unwrap();
        assert!(result.content.starts_with("<mjml>"));
    }
}
