pub mod loader;
pub mod parser;
pub mod render;

use std::sync::Arc;

use lettre::message::header::ContentType;
use lettre::message::Body;

#[derive(Clone, Debug, Default, serde::Deserialize)]
pub struct Config {
    #[serde(default)]
    pub loader: loader::Config,
    #[serde(default)]
    pub parser: parser::Config,
    #[serde(default)]
    pub render: render::Config,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("unable to interpolate template with provided variables: {0}")]
    Interpolation(#[from] handlebars::RenderError),
    #[error("unable to load tempalte from provider: {0}")]
    Loading(#[from] loader::Error),
    #[error("unable to parse template: {0}")]
    Parsing(#[from] mrml::prelude::parser::Error),
    #[error("unable to render template: {0}")]
    Rendering(#[from] mrml::prelude::render::Error),
    #[error("unable to build email: {0}")]
    Building(#[from] lettre::error::Error),
}

#[derive(Clone, Debug)]
pub struct Engine {
    loader: Arc<loader::Loader>,
    parser: Arc<mrml::prelude::parser::AsyncParserOptions>,
    render: Arc<mrml::prelude::render::RenderOptions>,
}

impl From<Config> for Engine {
    fn from(value: Config) -> Self {
        Self {
            loader: Arc::new(value.loader.into()),
            parser: Arc::new(value.parser.into()),
            render: Arc::new(value.render.into()),
        }
    }
}

impl Engine {
    async fn load(
        &self,
        name: &str,
    ) -> Result<
        catapulte_prelude::MetadataWithTemplate<catapulte_prelude::EmbeddedTemplateDefinition>,
        Error,
    > {
        self.loader.find_by_name(name).await.map_err(Error::Loading)
    }

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
            .map(|root| root.element)
            .map_err(Error::from)
    }

    fn render(&self, input: mrml::mjml::Mjml) -> Result<String, Error> {
        Ok(input.render(&self.render)?)
    }

    pub async fn handle(&self, req: Request) -> Result<lettre::Message, Error> {
        let metadata = self.load(req.name.as_str()).await?;
        // TODO handle embedded attachments
        // TODO validate params with jsonapi from template

        let res = self.interpolate(&metadata.template.content, &req.params)?;
        let res = self.parse(res).await?;

        let title = res.get_title();
        let preview = res.get_preview();
        let body = self.render(res)?;

        let msg = lettre::Message::builder().from(req.from);
        let msg = req.to.into_iter().fold(msg, |msg, item| msg.to(item));
        let msg = req.cc.into_iter().fold(msg, |msg, item| msg.cc(item));
        let msg = req.bcc.into_iter().fold(msg, |msg, item| msg.bcc(item));

        let multipart = match preview {
            Some(preview) => lettre::message::MultiPart::alternative_plain_html(preview, body),
            None => lettre::message::MultiPart::alternative()
                .singlepart(lettre::message::SinglePart::html(body)),
        };

        let multipart = req.attachments.into_iter().fold(multipart, |res, file| {
            res.singlepart(
                lettre::message::Attachment::new(file.filename)
                    .body(file.content, file.content_type),
            )
        });

        let msg = msg
            .subject(title.unwrap_or_default())
            .multipart(multipart)?;

        Ok(msg)
    }
}

#[derive(Debug)]
pub struct Attachment {
    pub filename: String,
    pub content_type: ContentType,
    pub content: Body,
}

#[derive(Debug)]
pub struct Request {
    pub name: String,
    pub from: lettre::message::Mailbox,
    pub to: Vec<lettre::message::Mailbox>,
    // carbon copy
    pub cc: Vec<lettre::message::Mailbox>,
    // blind carbon copy
    pub bcc: Vec<lettre::message::Mailbox>,
    pub params: serde_json::Value,
    pub attachments: Vec<Attachment>,
}

pub struct Message {
    pub title: Option<String>,
    pub preview: Option<String>,
    pub body: String,
}
