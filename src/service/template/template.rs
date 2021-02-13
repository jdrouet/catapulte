use crate::error::ServerError;
use crate::service::multipart::MultipartFile;
use handlebars::{Handlebars, TemplateRenderError as HandlebarTemplateRenderError};
use lettre::message::{Mailbox, Message, MessageBuilder, MultiPart, SinglePart};
use mrml::util::size::Size;
use mrml::Email;
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
}

impl From<lettre::error::Error> for TemplateError {
    fn from(err: lettre::error::Error) -> Self {
        Self::InvalidOptions(err.to_string())
    }
}

impl From<HandlebarTemplateRenderError> for TemplateError {
    fn from(err: HandlebarTemplateRenderError) -> Self {
        TemplateError::InterpolationError(err.to_string())
    }
}

impl From<TemplateError> for ServerError {
    fn from(err: TemplateError) -> Self {
        match err {
            TemplateError::InterpolationError(msg) => ServerError::BadRequest(msg),
            TemplateError::InvalidOptions(msg) => ServerError::BadRequest(msg),
            TemplateError::RenderingError(msg) => ServerError::InternalServerError(msg),
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
    pub fn to_builder(&self) -> MessageBuilder {
        let from: Mailbox = self.from.parse().unwrap();
        let builder = Message::builder().from(from);
        let builder = self.apply_to(builder);
        let builder = self.apply_cc(builder);
        let builder = self.apply_bcc(builder);
        builder
    }

    fn apply_to(&self, builder: MessageBuilder) -> MessageBuilder {
        self.to
            .iter()
            .filter_map(|address| address.parse::<Mailbox>().ok())
            .fold(builder, |b, address| b.to(address))
    }

    fn apply_cc(&self, builder: MessageBuilder) -> MessageBuilder {
        self.cc
            .iter()
            .filter_map(|address| address.parse::<Mailbox>().ok())
            .fold(builder, |b, address| b.cc(address))
    }

    fn apply_bcc(&self, builder: MessageBuilder) -> MessageBuilder {
        self.bcc
            .iter()
            .filter_map(|address| address.parse::<Mailbox>().ok())
            .fold(builder, |b, address| b.bcc(address))
    }
}

impl Template {
    fn render(&self, opts: &TemplateOptions) -> Result<mrml::Email, TemplateError> {
        let reg = Handlebars::new();
        let mjml = reg.render_template(self.content.as_str(), &opts.params)?;
        let email = mrml::to_email(mjml.as_str(), build_mrml_options())?;
        Ok(email)
    }

    fn get_body(email: &Email) -> MultiPart {
        MultiPart::alternative()
            .singlepart(
                SinglePart::builder()
                    .header(lettre::message::header::ContentType(
                        "text/plain; charset=utf8".parse().unwrap(),
                    ))
                    .body(email.text.to_string()),
            )
            .singlepart(
                SinglePart::builder()
                    .header(lettre::message::header::ContentType(
                        "text/html; charset=utf8".parse().unwrap(),
                    ))
                    .body(email.html.to_string()),
            )
    }

    fn add_attachment(builder: MultiPart, file: &MultipartFile) -> MultiPart {
        let mut parameters = vec![];
        if let Some(fname) = file.filename.as_ref() {
            parameters.push(lettre::message::header::DispositionParam::Filename(
                lettre::message::header::Charset::Ext("utf-8".into()),
                None,
                fname.as_bytes().to_vec(),
            ));
        }
        let body = lettre::message::Body::new(std::fs::read(file.filepath.clone()).unwrap());
        let part = SinglePart::builder()
            .header(lettre::message::header::ContentType(
                file.content_type.clone(),
            ))
            .header(lettre::message::header::ContentDisposition {
                disposition: lettre::message::header::DispositionType::Attachment,
                parameters,
            })
            .body(body);
        builder.singlepart(part)
    }

    fn get_multipart(&self, email: &Email, opts: &TemplateOptions) -> MultiPart {
        let builder = MultiPart::mixed();
        let builder = builder.multipart(Self::get_body(email));
        opts.attachments
            .iter()
            .fold(builder, |res, item| Self::add_attachment(res, item))
    }

    pub fn to_email(&self, opts: &TemplateOptions) -> Result<Message, TemplateError> {
        debug!("rendering template: {} ({})", self.name, self.description);
        let email = self.render(opts)?;
        let builder = opts.to_builder();
        Ok(builder
            .subject(email.subject.as_str())
            .multipart(self.get_multipart(&email, opts))?)
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
