pub mod parser;
pub mod render;

use std::sync::Arc;

#[derive(Clone, Debug, serde::Deserialize)]
pub struct Config {
    pub parser: parser::Config,
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

#[derive(Clone, Debug, Default)]
pub struct Engine {
    parser: Arc<mrml::prelude::parser::AsyncParserOptions>,
    render: Arc<mrml::prelude::render::RenderOptions>,
}

impl From<Config> for Engine {
    fn from(value: Config) -> Self {
        Self {
            parser: Arc::new(value.parser.into()),
            render: Arc::new(value.render.into()),
        }
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
        mrml::async_parse_with_options(input, self.parser.clone())
            .await
            .map_err(Error::from)
    }

    fn render(&self, input: mrml::mjml::Mjml) -> Result<String, Error> {
        Ok(input.render(&self.render)?)
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
