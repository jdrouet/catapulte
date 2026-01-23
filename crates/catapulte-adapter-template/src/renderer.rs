use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use catapulte_domain::error::RenderError;
use catapulte_domain::model::{RenderedEmail, Template};
use catapulte_domain::prelude::TemplateRenderer;

/// Configuration for MRML rendering
#[derive(Clone, Debug, Default, serde::Deserialize)]
pub struct MrmlRendererConfig {
    #[serde(default)]
    pub disable_comments: bool,
    #[serde(default)]
    pub social_icon_origin: Option<String>,
    #[serde(default)]
    pub fonts: Option<HashMap<String, String>>,
    #[serde(default)]
    pub include_loader: Vec<IncludeLoaderEntry>,
}

#[derive(Clone, Debug, Default, serde::Deserialize)]
#[serde(tag = "type")]
pub enum IncludeLoaderFilter {
    StartsWith {
        value: String,
    },
    #[default]
    Any,
}

#[derive(Clone, Debug, serde::Deserialize)]
pub struct IncludeLoaderEntry {
    pub filter: IncludeLoaderFilter,
    pub loader: IncludeLoaderConfig,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(tag = "type")]
pub enum IncludeLoaderConfig {
    Local { root: String },
    Memory { values: HashMap<String, String> },
}

/// MJML template renderer using mrml and handlebars
pub struct MrmlRenderer {
    parser_options: Arc<mrml::prelude::parser::AsyncParserOptions>,
    render_options: Arc<mrml::prelude::render::RenderOptions>,
}

impl MrmlRenderer {
    pub fn new(config: &MrmlRendererConfig) -> Self {
        Self {
            parser_options: Arc::new(Self::build_parser_options(config)),
            render_options: Arc::new(Self::build_render_options(config)),
        }
    }

    fn build_parser_options(
        config: &MrmlRendererConfig,
    ) -> mrml::prelude::parser::AsyncParserOptions {
        let mut loader = mrml::prelude::parser::multi_loader::MultiIncludeLoader::<
            Box<dyn mrml::prelude::parser::loader::AsyncIncludeLoader + Send + Sync + 'static>,
        >::new();

        for entry in &config.include_loader {
            let entry_loader: Box<
                dyn mrml::prelude::parser::loader::AsyncIncludeLoader + Send + Sync,
            > = match &entry.loader {
                IncludeLoaderConfig::Local { root } => Box::new(
                    mrml::prelude::parser::local_loader::LocalIncludeLoader::new(PathBuf::from(
                        root,
                    )),
                ),
                IncludeLoaderConfig::Memory { values } => {
                    Box::new(mrml::prelude::parser::memory_loader::MemoryIncludeLoader(
                        mrml::prelude::hash::Map::from_iter(values.clone()),
                    ))
                }
            };

            loader = match &entry.filter {
                IncludeLoaderFilter::Any => loader.with_any(entry_loader),
                IncludeLoaderFilter::StartsWith { value } => {
                    loader.with_starts_with(value.clone(), entry_loader)
                }
            };
        }

        // Add noop loader as fallback
        loader = loader
            .with_any(Box::<mrml::prelude::parser::noop_loader::NoopIncludeLoader>::default());

        mrml::prelude::parser::AsyncParserOptions {
            include_loader: Box::new(loader),
        }
    }

    fn build_render_options(config: &MrmlRendererConfig) -> mrml::prelude::render::RenderOptions {
        let mut options = mrml::prelude::render::RenderOptions {
            disable_comments: config.disable_comments,
            ..Default::default()
        };

        if let Some(origin) = &config.social_icon_origin {
            options.social_icon_origin = Some(origin.clone().into());
        }

        if let Some(fonts) = &config.fonts {
            options.fonts = fonts
                .iter()
                .map(|(key, value)| (key.clone(), value.clone().into()))
                .collect();
        }

        options
    }

    fn interpolate(
        &self,
        template: &str,
        params: &serde_json::Value,
    ) -> Result<String, RenderError> {
        let handlebars = handlebars::Handlebars::new();
        handlebars
            .render_template(template, params)
            .map_err(|err| RenderError::Interpolation(anyhow::Error::new(err)))
    }

    async fn parse(&self, input: String) -> Result<mrml::mjml::Mjml, RenderError> {
        mrml::async_parse_with_options(input, self.parser_options.clone())
            .await
            .map(|root| root.element)
            .map_err(|err| RenderError::Parse(anyhow::Error::new(err)))
    }

    fn render_html(&self, mjml: mrml::mjml::Mjml) -> Result<String, RenderError> {
        mjml.render(&self.render_options)
            .map_err(|err| RenderError::Render(anyhow::Error::new(err)))
    }
}

impl TemplateRenderer for MrmlRenderer {
    async fn render(
        &self,
        template: &Template,
        params: &serde_json::Value,
    ) -> Result<RenderedEmail, RenderError> {
        let interpolated = self.interpolate(&template.content, params)?;
        let mjml = self.parse(interpolated).await?;

        let subject = mjml.get_title().unwrap_or_default();
        let text_body = mjml.get_preview();
        let html_body = self.render_html(mjml)?;

        Ok(RenderedEmail {
            subject,
            text_body,
            html_body,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn should_render_template() {
        let config = MrmlRendererConfig::default();
        let renderer = MrmlRenderer::new(&config);

        let template = Template {
            metadata: catapulte_domain::model::TemplateMetadata {
                name: "test".to_string(),
                description: None,
                attributes: None,
            },
            content: r#"<mjml>
                <mj-head>
                    <mj-title>Hello {{name}}!</mj-title>
                    <mj-preview>Preview for {{name}}</mj-preview>
                </mj-head>
                <mj-body>
                    <mj-section>
                        <mj-column>
                            <mj-text>Hello {{name}}!</mj-text>
                        </mj-column>
                    </mj-section>
                </mj-body>
            </mjml>"#
                .to_string(),
        };

        let params = serde_json::json!({ "name": "World" });
        let rendered = renderer.render(&template, &params).await.unwrap();

        assert_eq!(rendered.subject, "Hello World!");
        assert_eq!(rendered.text_body.as_deref(), Some("Preview for World"));
        assert!(rendered.html_body.contains("Hello World!"));
    }

    #[tokio::test]
    async fn should_fail_on_invalid_template() {
        let config = MrmlRendererConfig::default();
        let renderer = MrmlRenderer::new(&config);

        let template = Template {
            metadata: catapulte_domain::model::TemplateMetadata {
                name: "test".to_string(),
                description: None,
                attributes: None,
            },
            content: "not valid mjml".to_string(),
        };

        let params = serde_json::json!({});
        let err = renderer.render(&template, &params).await.unwrap_err();
        assert!(matches!(err, RenderError::Parse(_)));
    }
}
