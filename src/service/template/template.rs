use crate::error::ServerError;
use handlebars::{Handlebars, TemplateRenderError as HandlebarTemplateRenderError};
use lettre::SendableEmail;
use lettre_email::{error::Error as LetterError, EmailBuilder};
use mrml;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::string::ToString;

#[derive(Debug)]
pub enum TemplateError {
    InterpolationError(HandlebarTemplateRenderError),
    RenderingError(mrml::Error),
    SendingError(LetterError),
}

impl From<HandlebarTemplateRenderError> for TemplateError {
    fn from(err: HandlebarTemplateRenderError) -> Self {
        TemplateError::InterpolationError(err)
    }
}

impl From<LetterError> for TemplateError {
    fn from(err: LetterError) -> Self {
        TemplateError::SendingError(err)
    }
}

impl From<TemplateError> for ServerError {
    fn from(err: TemplateError) -> Self {
        match err {
            TemplateError::InterpolationError(err) => ServerError::BadRequest(err.to_string()),
            TemplateError::RenderingError(err) => ServerError::InternalServerError(match err {
                mrml::Error::MJMLError(mjml_err) => match mjml_err {
                    mrml::mjml::error::Error::ParseError(msg) => msg.clone(),
                },
                mrml::Error::XMLError(xml_err) => xml_err.to_string(),
            }),
            TemplateError::SendingError(err) => ServerError::InternalServerError(err.to_string()),
        }
    }
}

impl From<mrml::Error> for TemplateError {
    fn from(err: mrml::Error) -> Self {
        TemplateError::RenderingError(err)
    }
}

impl std::fmt::Display for TemplateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Template {
    pub name: String,
    pub description: String,
    pub mjml: String,
}

#[derive(Debug, Deserialize)]
pub struct TemplateOptions {
    to: String,
    from: String,
    params: JsonValue,
}

impl Template {
    fn get_title(&self, _opts: &TemplateOptions) -> String {
        "".into()
    }

    fn get_text(&self, _opts: &TemplateOptions) -> String {
        "".into()
    }

    fn get_html(&self, opts: &TemplateOptions) -> Result<String, TemplateError> {
        let reg = Handlebars::new();
        let mjml = reg.render_template(self.mjml.as_str(), &opts.params)?;
        let html = mrml::to_html(mjml.as_str(), mrml::Options::default())?;
        Ok(html)
    }

    pub fn to_email(&self, opts: &TemplateOptions) -> Result<SendableEmail, TemplateError> {
        debug!("rendering template: {} ({})", self.name, self.description);
        let email = EmailBuilder::new()
            .from(opts.from.clone())
            .to(opts.to.clone())
            .subject(self.get_title(&opts))
            .text(self.get_text(&opts))
            .html(self.get_html(&opts)?)
            .build()?;
        Ok(email.into())
    }
}
