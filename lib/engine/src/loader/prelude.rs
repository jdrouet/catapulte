#[derive(Debug, serde::Deserialize)]
pub struct Template {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub content: String,
    pub attributes: serde_json::Value,
}
