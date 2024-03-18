pub mod render;

use std::{rc::Rc, sync::Arc};

#[derive(Clone, Debug, serde::Deserialize)]
pub struct Config {
    // pub parser: mrml::prelude::parser::ParserOptions,
    pub render: render::Config,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("unable to interpolate template with provided variables: {0}")]
    Interpolation(#[from] handlebars::RenderError),
    #[error("unable to parse template: {0}")]
    Parsing(#[from] mrml::prelude::parser::Error),
    #[error("unable to render template: {0}")]
    Rendering(#[from] mrml::prelude::render::Error),
}

#[derive(Debug, Default)]
struct InnerEngine {
    parser: Rc<mrml::prelude::parser::AsyncParserOptions>,
    render: mrml::prelude::render::RenderOptions,
}

#[derive(Clone, Debug, Default)]
pub struct Engine(Arc<InnerEngine>);

impl From<Config> for Engine {
    fn from(value: Config) -> Self {
        Self(Arc::new(InnerEngine {
            parser: Default::default(),
            render: value.render.into(),
        }))
    }
}

impl Engine {
    fn interpolate<T>(&self, input: &str, params: &T) -> Result<String, Error>
    where
        T: serde::Serialize,
    {
        let handlebar = handlebars::Handlebars::new();
        Ok(handlebar.render_template(input, params)?)
    }

    async fn parse(&self, input: String) -> Result<mrml::mjml::Mjml, Error> {
        mrml::async_parse_with_options(input, self.0.parser.clone())
            .await
            .map_err(Error::from)
    }

    fn render(&self, input: mrml::mjml::Mjml) -> Result<String, Error> {
        Ok(input.render(&self.0.render)?)
    }

    pub async fn handle<T>(
        &self,
        input: &str,
        params: &T,
    ) -> Result<(Option<String>, Option<String>, String), Error>
    where
        T: serde::Serialize,
    {
        let res = self.interpolate(input, params)?;
        let res = self.parse(res).await?;
        let title = res.get_title();
        let preview = res.get_preview();
        let body = self.render(res)?;
        Ok((title, preview, body))
    }
}
