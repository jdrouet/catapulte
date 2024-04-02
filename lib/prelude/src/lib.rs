use std::path::PathBuf;

#[derive(Debug, serde::Deserialize, serde::Serialize)]
pub struct Metadata {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attributes: Option<serde_json::Value>,
}

#[derive(Debug, serde::Deserialize, serde::Serialize)]
pub struct MetadataWithTemplate<T = TemplateDefinition> {
    #[serde(flatten)]
    pub inner: Metadata,
    pub template: T,
}

#[derive(Debug, serde::Deserialize, serde::Serialize)]
pub struct LocalTemplateDefinition {
    pub path: std::path::PathBuf,
}

impl Default for LocalTemplateDefinition {
    fn default() -> Self {
        Self {
            path: PathBuf::from("template.mjml"),
        }
    }
}

#[derive(Debug, serde::Deserialize, serde::Serialize)]
pub struct EmbeddedTemplateDefinition {
    pub content: String,
}

#[derive(Debug, serde::Deserialize, serde::Serialize)]
pub struct RemoteTemplateDefinition {
    pub url: url::Url,
}

#[derive(Debug, serde::Deserialize, serde::Serialize)]
#[serde(untagged)]
pub enum TemplateDefinition {
    Local(LocalTemplateDefinition),
    Embedded(EmbeddedTemplateDefinition),
    Remote(RemoteTemplateDefinition),
}

impl Default for TemplateDefinition {
    fn default() -> Self {
        Self::Local(LocalTemplateDefinition::default())
    }
}

#[derive(Debug, serde::Deserialize, serde::Serialize)]
#[serde(untagged)]
pub enum Either<A, B> {
    First(A),
    Second(B),
}
