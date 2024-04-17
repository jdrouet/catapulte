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

#[cfg(test)]
mod tests {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn should_find_template_locally() {
        let config: super::Config =
            serde_json::from_str(r#"{ "local": { "path": "../../template" } }"#).unwrap();
        let loader = super::Loader::from(config);
        let found = loader.find_by_name("user-login").await.unwrap();
        assert_eq!(found.inner.name, "user-login");
    }

    #[tokio::test]
    async fn should_find_template_remotely() {
        let content = include_str!("../../../../template/user-login/template.mjml");
        let metadata = serde_json::json!({
            "name": "user-login",
            "description": "read from single file",
            "template": {
                "content": content,
            },
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
        let config: super::Config = serde_json::from_value(serde_json::json!({
            "local": {
                "path": "./not-found",
            },
            "http": {
                "url": format!("{}/templates/", mock_server.uri())
            }
        }))
        .unwrap();
        Mock::given(method("GET"))
            .and(path("/templates/user-login/metadata.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(metadata))
            .mount(&mock_server)
            .await;

        let loader = super::Loader::from(config);
        let found = loader.find_by_name("user-login").await.unwrap();
        assert_eq!(found.inner.name, "user-login");
    }

    #[tokio::test]
    async fn should_not_find_template() {
        let mock_server = MockServer::start().await;
        let config: super::Config = serde_json::from_value(serde_json::json!({
            "local": {
                "path": "./not-found",
            },
            "http": {
                "url": format!("{}/templates/", mock_server.uri())
            }
        }))
        .unwrap();
        //
        let loader = super::Loader::from(config);
        let err = loader.find_by_name("user-login").await.unwrap_err();
        assert!(matches!(err, super::Error::Multiple(_)));
    }
}
