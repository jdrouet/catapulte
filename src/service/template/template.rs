use crate::error::ServerError;
use crate::service::multipart::MultipartFile;
use handlebars::{Handlebars, TemplateRenderError as HandlebarTemplateRenderError};
use lettre::SendableEmail;
use lettre_email::{error::Error as LetterError, EmailBuilder};
use mrml::util::size::Size;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::str::FromStr;
use std::string::ToString;

fn build_mrml_options() -> mrml::Options {
    let mut result = mrml::Options::default();
    if let Some(value) = std::env::var("MRML_BREAKPOINT")
        .ok()
        .and_then(|breakpoint| Size::from_str(breakpoint.as_str()).ok())
    {
        result.breakpoint = value;
    }
    if let Some(value) = std::env::var("MRML_KEEP_COMMENTS")
        .ok()
        .and_then(|keep_comments| keep_comments.parse::<bool>().ok())
    {
        result.keep_comments = value;
    }
    if let Ok(value) = std::env::var("MRML_SOCIAL_ICON_ORIGIN") {
        result.social_icon_origin = value;
    }
    result
}

#[derive(Clone, Debug)]
pub enum TemplateError {
    InterpolationError(String),
    InvalidOptions(String),
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
            TemplateError::InvalidOptions(msg) => ServerError::BadRequest(msg),
            TemplateError::RenderingError(msg) => ServerError::InternalServerError(msg),
            TemplateError::SendingError(msg) => ServerError::InternalServerError(msg),
        }
    }
}

impl From<mrml::Error> for TemplateError {
    fn from(err: mrml::Error) -> Self {
        let msg = match err {
            mrml::Error::MJMLError(mjml_error) => format!("MJML Error: {:?}", mjml_error),
            mrml::Error::ParserError(parser_error) => format!("Parser Error: {:?}", parser_error),
        };
        TemplateError::RenderingError(msg)
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Template {
    pub name: String,
    #[serde(default = "String::new")]
    pub description: String,
    pub content: String,
    pub attributes: JsonValue,
}

pub fn default_attachments() -> Vec<MultipartFile> {
    vec![]
}

#[derive(Debug, Deserialize)]
pub struct TemplateOptions {
    to: Vec<String>,
    cc: Vec<String>,
    bcc: Vec<String>,
    from: String,
    params: JsonValue,
    #[serde(default = "default_attachments", skip_deserializing, skip_serializing)]
    attachments: Vec<MultipartFile>,
}

impl TemplateOptions {
    pub fn new(
        from: String,
        to: Vec<String>,
        cc: Vec<String>,
        bcc: Vec<String>,
        params: JsonValue,
        attachments: Vec<MultipartFile>,
    ) -> Self {
        Self {
            from,
            to,
            cc,
            bcc,
            params,
            attachments,
        }
    }

    pub fn validate(&self) -> Result<(), TemplateError> {
        if self.from.is_empty() {
            Err(TemplateError::InvalidOptions(
                "missing \"from\" field".into(),
            ))
        } else if self.to.is_empty() && self.cc.is_empty() && self.bcc.is_empty() {
            Err(TemplateError::InvalidOptions(
                "missing \"to\", \"cc\" and \"bcc\"".into(),
            ))
        } else {
            Ok(())
        }
    }
}

impl TemplateOptions {
    pub fn to_builder(&self) -> EmailBuilder {
        let mut builder = EmailBuilder::new().from(self.from.as_str());
        builder = self.apply_to(builder);
        builder = self.apply_cc(builder);
        builder = self.apply_bcc(builder);
        builder
    }

    fn apply_to(&self, builder: EmailBuilder) -> EmailBuilder {
        self.to
            .iter()
            .fold(builder, |b, address| b.to(address.as_str()))
    }

    fn apply_cc(&self, builder: EmailBuilder) -> EmailBuilder {
        self.cc
            .iter()
            .fold(builder, |b, address| b.cc(address.as_str()))
    }

    fn apply_bcc(&self, builder: EmailBuilder) -> EmailBuilder {
        self.bcc
            .iter()
            .fold(builder, |b, address| b.bcc(address.as_str()))
    }
}

impl Template {
    fn render(&self, opts: &TemplateOptions) -> Result<mrml::Email, TemplateError> {
        let reg = Handlebars::new();
        let mjml = reg.render_template(self.content.as_str(), &opts.params)?;
        let email = mrml::to_email(mjml.as_str(), build_mrml_options())?;
        Ok(email)
    }

    pub fn to_email(&self, opts: &TemplateOptions) -> Result<SendableEmail, TemplateError> {
        debug!("rendering template: {} ({})", self.name, self.description);
        let email = self.render(opts)?;
        let mut builder = opts
            .to_builder()
            .subject(email.subject)
            .text(email.text)
            .html(email.html);
        for item in opts.attachments.iter() {
            builder = builder.attachment_from_file(
                item.filepath.as_path(),
                item.filename.as_deref(),
                &item.content_type,
            )?;
        }
        let email = builder.build()?;
        Ok(email.into())
    }
}

// LCOV_EXCL_START
#[cfg(test)]
mod tests {
    use super::*;
    use env_test_util::TempEnvVar;
    use mrml::Options;
    use serde_json::json;

    #[test]
    #[serial]
    fn building_mrml_options_breakpoint_unset() {
        let _breakpoint = TempEnvVar::new("MRML_BREAKPOINT");
        let result = build_mrml_options();
        assert_eq!(
            result.breakpoint.to_string(),
            Options::default().breakpoint.to_string()
        );
    }
    #[test]
    #[serial]
    fn building_mrml_options_breakpoint_set() {
        let _breakpoint = TempEnvVar::new("MRML_BREAKPOINT").with("800px");
        let result = build_mrml_options();
        assert_eq!(result.breakpoint.to_string(), "800px");
    }

    #[test]
    #[serial]
    fn building_mrml_options_breakpoint_invalid() {
        let _breakpoint = TempEnvVar::new("MRML_BREAKPOINT").with("invalid");
        let result = build_mrml_options();
        assert_eq!(
            result.breakpoint.to_string(),
            Options::default().breakpoint.to_string()
        );
    }

    #[test]
    #[serial]
    fn building_mrml_options_keep_comments_unset() {
        let _breakpoint = TempEnvVar::new("MRML_KEEP_COMMENTS");
        let result = build_mrml_options();
        assert_eq!(result.keep_comments, Options::default().keep_comments);
    }
    #[test]
    #[serial]
    fn building_mrml_options_keep_comments_set() {
        let _breakpoint = TempEnvVar::new("MRML_KEEP_COMMENTS").with("TrUe");
        let result = build_mrml_options();
        assert_eq!(result.keep_comments, true);
    }

    #[test]
    #[serial]
    fn building_mrml_options_keep_comments_invalid() {
        let _breakpoint = TempEnvVar::new("MRML_KEEP_COMMENTS").with("invalid");
        let result = build_mrml_options();
        assert_eq!(result.keep_comments, Options::default().keep_comments);
    }

    #[test]
    #[serial]
    fn building_mrml_options_social_icon_origin_unset() {
        let _breakpoint = TempEnvVar::new("MRML_SOCIAL_ICON_ORIGIN");
        let result = build_mrml_options();
        assert_eq!(
            result.social_icon_origin,
            Options::default().social_icon_origin
        );
    }
    #[test]
    #[serial]
    fn building_mrml_options_social_icon_origin_set() {
        let _breakpoint = TempEnvVar::new("MRML_SOCIAL_ICON_ORIGIN").with("http://wherever.com/");
        let result = build_mrml_options();
        assert_eq!(result.social_icon_origin, "http://wherever.com/");
    }

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
            vec!["recipient@example.com".into()],
            vec![],
            vec![],
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
            vec!["recipient@example.com".into()],
            vec![],
            vec![],
            json!({"name": "Alice"}),
            vec![],
        );
        let result = tmpl.to_email(&opts);
        assert!(result.is_ok());
    }
}
// LCOV_EXCL_END
