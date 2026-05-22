use anyhow::Context;
use catapulte_domain::entity::body::{InterpolatedBody, Plain, RenderedBody};
use catapulte_domain::port::template_renderer::{RenderError, TemplateRenderer};

#[derive(Debug, Default)]
pub struct MjmlRenderer;

impl MjmlRenderer {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl TemplateRenderer for MjmlRenderer {
    /// # Errors
    ///
    /// Returns a `RenderError` when the mjml source fails to parse or render.
    fn render(&self, body: InterpolatedBody) -> Result<RenderedBody, RenderError> {
        match body {
            InterpolatedBody::Plain(plain) => Ok(RenderedBody::new(plain)),
            InterpolatedBody::Mjml(source) => render_mjml(&source),
        }
    }
}

fn render_mjml(source: &str) -> Result<RenderedBody, RenderError> {
    let parsed = mrml::parse(source)
        .context("failed to parse mjml")
        .map_err(|source| RenderError::Mjml { source })?;

    let opts = mrml::prelude::render::RenderOptions::default();
    let html = parsed
        .element
        .render(&opts)
        .context("failed to render mjml")
        .map_err(|source| RenderError::Mjml { source })?;

    let preview = parsed.element.get_preview();

    let plain = Plain::try_new(preview, Some(html))
        .context("rendered mjml has no body parts")
        .map_err(|source| RenderError::Mjml { source })?;

    Ok(RenderedBody::new(plain))
}

#[cfg(test)]
mod tests {
    use catapulte_domain::entity::body::{InterpolatedBody, Plain};
    use catapulte_domain::port::template_renderer::{RenderError, TemplateRenderer};

    use super::MjmlRenderer;

    #[test]
    fn render_plain_pass_through_text_only() {
        let renderer = MjmlRenderer::new();
        let plain = Plain::try_new(Some("hi".to_string()), None).unwrap();
        let body = InterpolatedBody::Plain(plain);
        let result = renderer.render(body).unwrap();
        let output = result.into_plain();
        assert_eq!(output.text(), Some("hi"));
        assert_eq!(output.html(), None);
    }

    #[test]
    fn render_plain_pass_through_both() {
        let renderer = MjmlRenderer::new();
        let plain =
            Plain::try_new(Some("text".to_string()), Some("<p>html</p>".to_string())).unwrap();
        let body = InterpolatedBody::Plain(plain);
        let result = renderer.render(body).unwrap();
        let output = result.into_plain();
        assert_eq!(output.text(), Some("text"));
        assert_eq!(output.html(), Some("<p>html</p>"));
    }

    #[test]
    fn render_mjml_with_preview_produces_text_and_html() {
        let renderer = MjmlRenderer::new();
        let source = r#"<mjml>
  <mj-head>
    <mj-preview>preview text</mj-preview>
  </mj-head>
  <mj-body>
    <mj-section>
      <mj-column>
        <mj-text>Hello world</mj-text>
      </mj-column>
    </mj-section>
  </mj-body>
</mjml>"#;
        let body = InterpolatedBody::Mjml(source.to_string());
        let result = renderer.render(body).unwrap();
        let output = result.into_plain();
        assert_eq!(output.text(), Some("preview text"));
        let html = output.html().unwrap();
        assert!(!html.is_empty());
        assert!(html.contains("Hello world"));
    }

    #[test]
    fn render_mjml_without_preview_produces_html_only() {
        let renderer = MjmlRenderer::new();
        let source = r#"<mjml>
  <mj-body>
    <mj-section>
      <mj-column>
        <mj-text>Hello world</mj-text>
      </mj-column>
    </mj-section>
  </mj-body>
</mjml>"#;
        let body = InterpolatedBody::Mjml(source.to_string());
        let result = renderer.render(body).unwrap();
        let output = result.into_plain();
        assert_eq!(output.text(), None);
        let html = output.html().unwrap();
        assert!(!html.is_empty());
        assert!(html.contains("Hello world"));
    }

    #[test]
    fn render_invalid_mjml_returns_render_error() {
        let renderer = MjmlRenderer::new();
        let body = InterpolatedBody::Mjml("<not mjml".to_string());
        let result = renderer.render(body);
        assert!(matches!(result, Err(RenderError::Mjml { .. })));
    }
}
