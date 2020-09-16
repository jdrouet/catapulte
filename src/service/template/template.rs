use crate::error::ServerError;
use crate::service::multipart::MultipartFile;
use handlebars::{Handlebars, TemplateRenderError as HandlebarTemplateRenderError};
use lettre::SendableEmail;
use lettre_email::{error::Error as LetterError, EmailBuilder};
use mrml;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::string::ToString;

#[derive(Clone, Debug)]
pub enum TemplateError {
    InterpolationError(String),
    RenderingError(String),
    SendingError(String),
}

impl From<HandlebarTemplateRenderError> for TemplateError {
    fn from(err: HandlebarTemplateRenderError) -> Self {
        TemplateError::InterpolationError(err.to_string())
    }
}

impl From<LetterError> for TemplateError {
    fn from(err: LetterError) -> Self {
        TemplateError::SendingError(err.to_string())
    }
}

impl From<TemplateError> for ServerError {
    fn from(err: TemplateError) -> Self {
        match err {
            TemplateError::InterpolationError(msg) => ServerError::BadRequest(msg),
            TemplateError::RenderingError(msg) => ServerError::InternalServerError(msg),
            TemplateError::SendingError(msg) => ServerError::InternalServerError(msg),
        }
    }
}

impl From<mrml::Error> for TemplateError {
    fn from(err: mrml::Error) -> Self {
        let msg = match err {
            mrml::Error::MJMLError(mjml_error) => match mjml_error {
                mrml::mjml::error::Error::ParseError(msg) => format!("parser error: {}", msg),
            },
            mrml::Error::XMLError(xml_error) => xml_error.to_string(),
        };
        TemplateError::RenderingError(msg)
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Template {
    pub name: String,
    pub description: String,
    pub content: String,
    pub attributes: JsonValue,
}

pub fn default_attachments() -> Vec<MultipartFile> {
    vec![]
}

#[derive(Debug, Deserialize)]
pub struct TemplateOptions {
    to: String,
    from: String,
    params: JsonValue,
    #[serde(default = "default_attachments", skip_deserializing, skip_serializing)]
    attachments: Vec<MultipartFile>,
}

impl TemplateOptions {
    pub fn new(
        from: String,
        to: String,
        params: JsonValue,
        attachments: Vec<MultipartFile>,
    ) -> Self {
        Self {
            from,
            to,
            params,
            attachments,
        }
    }
}

impl Template {
    fn render(&self, opts: &TemplateOptions) -> Result<mrml::Email, TemplateError> {
        let reg = Handlebars::new();
        let mjml = reg.render_template(self.content.as_str(), &opts.params)?;
        let email = mrml::to_email(mjml.as_str(), mrml::Options::default())?;
        Ok(email)
    }

    pub fn to_email(&self, opts: &TemplateOptions) -> Result<SendableEmail, TemplateError> {
        debug!("rendering template: {} ({})", self.name, self.description);
        let email = self.render(opts)?;
        let mut builder = EmailBuilder::new()
            .from(opts.from.clone())
            .to(opts.to.clone())
            .subject(email.subject)
            .text(email.text)
            .html(email.html);
        for item in opts.attachments.iter() {
            builder = builder.attachment_from_file(
                item.filepath.as_path(),
                item.filename.as_ref().map(|value| value.as_str()),
                &item.content_type,
            )?;
        }
        let email = builder.build()?;
        Ok(email.into())
    }
}

#[cfg(not(tarpaulin_include))]
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn render_success() {
        let tmpl = Template {
            name: "hello".into(),
            description: "world".into(),
            content: "<mjml></mjml>".into(),
            attributes: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string"
                    }
                },
            }),
        };
        let opts = TemplateOptions::new(
            "sender@example.com".into(),
            "recipient@example.com".into(),
            json!({"name": "Alice"}),
            vec![],
        );
        let result = tmpl.render(&opts);
        assert!(result.is_ok());
    }

    #[test]
    fn to_email_success() {
        let tmpl = Template {
            name: "hello".into(),
            description: "world".into(),
            content: "<mjml></mjml>".into(),
            attributes: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string"
                    }
                },
            }),
        };
        let opts = TemplateOptions::new(
            "sender@example.com".into(),
            "recipient@example.com".into(),
            json!({"name": "Alice"}),
            vec![],
        );
        let result = tmpl.to_email(&opts);
        assert!(result.is_ok());
    }
}
