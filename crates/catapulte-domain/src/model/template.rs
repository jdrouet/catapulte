/// Metadata associated with a template
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct TemplateMetadata {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attributes: Option<serde_json::Value>,
}

/// A loaded template ready for rendering
#[derive(Debug, Clone)]
pub struct Template {
    pub metadata: TemplateMetadata,
    pub content: String,
}
