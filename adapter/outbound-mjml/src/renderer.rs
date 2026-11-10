use std::sync::Arc;

use anyhow::Context;
use catapulte_domain::entity::body::{InterpolatedBody, Plain, RenderedBody};
use catapulte_domain::port::template_renderer::{RenderError, TemplateRenderer};
use mrml::prelude::parser::AsyncParserOptions;
use mrml::prelude::parser::loader::AsyncIncludeLoader;

pub struct MjmlRenderer {
    parser_options: Arc<AsyncParserOptions>,
}

impl MjmlRenderer {
    #[must_use]
    pub fn new(include_loader: Box<dyn AsyncIncludeLoader + Send + Sync>) -> Self {
        Self {
            parser_options: Arc::new(AsyncParserOptions { include_loader }),
        }
    }
}

impl TemplateRenderer for MjmlRenderer {
    /// # Errors
    ///
    /// Returns a `RenderError` when the mjml source fails to parse or render.
    #[tracing::instrument(skip_all, name = "template.render")]
    async fn render(&self, body: InterpolatedBody) -> Result<RenderedBody, RenderError> {
        match body {
            InterpolatedBody::Plain(plain) => Ok(RenderedBody::new(plain)),
            InterpolatedBody::Mjml(source) => {
                render_mjml(&source, Arc::clone(&self.parser_options)).await
            }
        }
    }
}

async fn render_mjml(
    source: &str,
    opts: Arc<AsyncParserOptions>,
) -> Result<RenderedBody, RenderError> {
    let parsed = mrml::async_parse_with_options(source, opts)
        .await
        .context("failed to parse mjml")
        .map_err(|source| RenderError::Mjml { source })?;

    let render_opts = mrml::prelude::render::RenderOptions::default();
    let html = parsed
        .element
        .render(&render_opts)
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
    use mrml::prelude::parser::noop_loader::NoopIncludeLoader;

    use super::MjmlRenderer;

    fn noop_renderer() -> MjmlRenderer {
        MjmlRenderer::new(Box::new(NoopIncludeLoader))
    }

    #[tokio::test]
    async fn render_plain_pass_through_text_only() {
        let renderer = noop_renderer();
        let plain = Plain::try_new(Some("hi".to_string()), None).unwrap();
        let body = InterpolatedBody::Plain(plain);
        let result = renderer.render(body).await.unwrap();
        let output = result.into_plain();
        assert_eq!(output.text(), Some("hi"));
        assert_eq!(output.html(), None);
    }

    #[tokio::test]
    async fn render_plain_pass_through_both() {
        let renderer = noop_renderer();
        let plain =
            Plain::try_new(Some("text".to_string()), Some("<p>html</p>".to_string())).unwrap();
        let body = InterpolatedBody::Plain(plain);
        let result = renderer.render(body).await.unwrap();
        let output = result.into_plain();
        assert_eq!(output.text(), Some("text"));
        assert_eq!(output.html(), Some("<p>html</p>"));
    }

    #[tokio::test]
    async fn render_mjml_with_preview_produces_text_and_html() {
        let renderer = noop_renderer();
        let source = r"<mjml>
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
</mjml>";
        let body = InterpolatedBody::Mjml(source.to_string());
        let result = renderer.render(body).await.unwrap();
        let output = result.into_plain();
        assert_eq!(output.text(), Some("preview text"));
        let html = output.html().unwrap();
        assert!(!html.is_empty());
        assert!(html.contains("Hello world"));
    }

    #[tokio::test]
    async fn render_mjml_without_preview_produces_html_only() {
        let renderer = noop_renderer();
        let source = r"<mjml>
  <mj-body>
    <mj-section>
      <mj-column>
        <mj-text>Hello world</mj-text>
      </mj-column>
    </mj-section>
  </mj-body>
</mjml>";
        let body = InterpolatedBody::Mjml(source.to_string());
        let result = renderer.render(body).await.unwrap();
        let output = result.into_plain();
        assert_eq!(output.text(), None);
        let html = output.html().unwrap();
        assert!(!html.is_empty());
        assert!(html.contains("Hello world"));
    }

    #[tokio::test]
    async fn render_invalid_mjml_returns_render_error() {
        let renderer = noop_renderer();
        let body = InterpolatedBody::Mjml("<not mjml".to_string());
        let result = renderer.render(body).await;
        assert!(matches!(result, Err(RenderError::Mjml { .. })));
    }

    #[tokio::test]
    async fn render_with_local_include_resolves() {
        use mrml::prelude::parser::local_loader::LocalIncludeLoader;
        use std::io::Write;

        let dir = tempfile::tempdir().unwrap();
        // Canonicalize so the starts_with check inside LocalIncludeLoader
        // works even on macOS where /var is a symlink to /private/var.
        let root = dir.path().canonicalize().unwrap();
        let header_path = root.join("header.mjml");
        std::fs::File::create(&header_path)
            .unwrap()
            .write_all(b"<mj-text>Hello include</mj-text>")
            .unwrap();

        let loader = LocalIncludeLoader::new(root);
        let renderer = MjmlRenderer::new(Box::new(loader));

        let source = r#"<mjml>
  <mj-body>
    <mj-section>
      <mj-column>
        <mj-include path="file:///header.mjml" />
      </mj-column>
    </mj-section>
  </mj-body>
</mjml>"#;
        let body = InterpolatedBody::Mjml(source.to_string());
        let result = renderer.render(body).await.unwrap();
        let html = result.into_plain().html().unwrap().to_string();
        assert!(
            html.contains("Hello include"),
            "expected 'Hello include' in rendered HTML, got: {html}"
        );
    }

    #[tokio::test]
    async fn render_with_local_include_path_traversal_fails() {
        use mrml::prelude::parser::local_loader::LocalIncludeLoader;

        let dir = tempfile::tempdir().unwrap();
        let loader = LocalIncludeLoader::new(dir.path().to_path_buf());
        let renderer = MjmlRenderer::new(Box::new(loader));

        let source = r#"<mjml>
  <mj-body>
    <mj-section>
      <mj-column>
        <mj-include path="file:///../etc/passwd" />
      </mj-column>
    </mj-section>
  </mj-body>
</mjml>"#;
        let body = InterpolatedBody::Mjml(source.to_string());
        let result = renderer.render(body).await;
        assert!(matches!(result, Err(RenderError::Mjml { .. })));
    }

    #[tokio::test]
    async fn render_without_include_loader_errors_on_mj_include() {
        let renderer = noop_renderer();
        let source = r#"<mjml>
  <mj-body>
    <mj-section>
      <mj-column>
        <mj-include path="file:///some.mjml" />
      </mj-column>
    </mj-section>
  </mj-body>
</mjml>"#;
        let body = InterpolatedBody::Mjml(source.to_string());
        let result = renderer.render(body).await;
        assert!(
            matches!(result, Err(RenderError::Mjml { .. })),
            "expected RenderError from noop loader, got Ok"
        );
    }

    #[tokio::test]
    async fn render_with_http_include_resolves_via_built_loader() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string("<mj-text>Hello http</mj-text>"),
            )
            .mount(&server)
            .await;
        let vars = std::collections::HashMap::from([(
            "CATAPULTE_INCLUDE_LOADER_HTTP_ALLOW".to_owned(),
            server.uri(),
        )]);
        let config = crate::include_loader::IncludeLoaderConfig::from_lookup(
            "CATAPULTE_INCLUDE_LOADER",
            |k| vars.get(k).cloned().ok_or(std::env::VarError::NotPresent),
        )
        .unwrap();
        let renderer = MjmlRenderer::new(config.build().unwrap());

        let source = format!(
            r#"<mjml><mj-body><mj-section><mj-column><mj-include path="{}/h.mjml" /></mj-column></mj-section></mj-body></mjml>"#,
            server.uri()
        );
        let result = renderer
            .render(InterpolatedBody::Mjml(source))
            .await
            .unwrap();

        assert!(result.into_plain().html().unwrap().contains("Hello http"));
    }

    #[tokio::test]
    async fn nested_include_to_disallowed_origin_sends_no_request() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let target = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("<mj-text>no</mj-text>"))
            .expect(0)
            .mount(&target)
            .await;
        let allowed = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string(format!(
                r#"<mj-text>outer</mj-text><mj-include path="{}/n.mjml" />"#,
                target.uri()
            )))
            .expect(1)
            .mount(&allowed)
            .await;
        let vars = std::collections::HashMap::from([(
            "CATAPULTE_INCLUDE_LOADER_HTTP_ALLOW".to_owned(),
            allowed.uri(),
        )]);
        let config = crate::include_loader::IncludeLoaderConfig::from_lookup(
            "CATAPULTE_INCLUDE_LOADER",
            |k| vars.get(k).cloned().ok_or(std::env::VarError::NotPresent),
        )
        .unwrap();
        let renderer = MjmlRenderer::new(config.build().unwrap());
        let source = format!(
            r#"<mjml><mj-body><mj-section><mj-column><mj-include path="{}/o.mjml" /></mj-column></mj-section></mj-body></mjml>"#,
            allowed.uri()
        );

        let result = renderer.render(InterpolatedBody::Mjml(source)).await;

        assert!(matches!(result, Err(RenderError::Mjml { .. })));
    }
}
