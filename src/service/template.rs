use crate::error::ServerError;
use crate::service::multipart::MultipartFile;
use handlebars::{Handlebars, RenderError as HandlebarTemplateRenderError};
use lettre::error::Error as LettreError;
use lettre::message::{Attachment, Body, Mailbox, Message, MessageBuilder, MultiPart, SinglePart};
use mrml::mjml::Mjml;
use mrml::prelude::parser::Error as ParserError;
use mrml::prelude::render::{Error as RenderError, RenderOptions};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};
use std::string::ToString;

#[derive(Debug)]
pub enum TemplateError {
    InterpolationError(HandlebarTemplateRenderError),
    InvalidOptions(LettreError),
    RenderingError(RenderError),
    ParsingError(ParserError),
}

impl From<LettreError> for TemplateError {
    fn from(err: LettreError) -> Self {
        Self::InvalidOptions(err)
    }
}

impl From<HandlebarTemplateRenderError> for TemplateError {
    fn from(err: HandlebarTemplateRenderError) -> Self {
        TemplateError::InterpolationError(err)
    }
}

impl From<TemplateError> for ServerError {
    fn from(err: TemplateError) -> Self {
        match err {
            TemplateError::InterpolationError(err) => {
                ServerError::bad_request("template interpolation error").details(json!({
                    "origin": "template",
                    "description": err.desc,
                    "template": err.template_name,
                    "line": err.line_no,
                    "column": err.column_no,
                }))
            }
            TemplateError::InvalidOptions(err) => {
                ServerError::bad_request("template rendering options invalid").details(json!({
                    "origin": "template",
                    "description": err.to_string(),
                }))
            }
            TemplateError::RenderingError(err) => ServerError::internal()
                .message("template rendering failed")
                .details(json!({
                    "origin": "template",
                    "description": err.to_string(),
                })),
            TemplateError::ParsingError(err) => ServerError::internal()
                .message("template parsing failed")
                .details(json!({
                    "origin": "template",
                    "description": err.to_string(),
                })),
        }
    }
}

impl From<ParserError> for TemplateError {
    fn from(err: ParserError) -> Self {
        TemplateError::ParsingError(err)
    }
}

impl From<RenderError> for TemplateError {
    fn from(err: RenderError) -> Self {
        TemplateError::RenderingError(err)
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Template {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub content: String,
    pub attributes: JsonValue,
}

#[derive(Debug, Deserialize)]
pub struct TemplateOptions {
    to: Vec<String>,
    cc: Vec<String>,
    bcc: Vec<String>,
    from: String,
    params: JsonValue,
    #[serde(default, skip_deserializing, skip_serializing)]
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
            to,
            cc,
            bcc,
            from,
            params,
            attachments,
        }
    }

    pub fn validate(&self) -> Result<(), TemplateError> {
        if self.from.is_empty() {
            Err(TemplateError::InvalidOptions(LettreError::MissingFrom))
        } else if self.to.is_empty() && self.cc.is_empty() && self.bcc.is_empty() {
            Err(TemplateError::InvalidOptions(LettreError::MissingTo))
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
        self.apply_bcc(builder)
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
    fn render(&self, opts: &TemplateOptions) -> Result<Mjml, TemplateError> {
        let reg = Handlebars::new();
        let mjml = reg.render_template(self.content.as_str(), &opts.params)?;
        Ok(mrml::parse(mjml)?)
    }

    fn get_body(email: &Mjml, opts: &RenderOptions) -> Result<MultiPart, TemplateError> {
        Ok(MultiPart::alternative()
            .singlepart(Self::get_body_plain(email))
            .multipart(Self::get_body_html(email, opts)?))
    }

    fn get_body_plain(email: &Mjml) -> SinglePart {
        SinglePart::plain(email.get_preview().unwrap_or_default())
    }

    fn get_body_html(email: &Mjml, opts: &RenderOptions) -> Result<MultiPart, TemplateError> {
        Ok(MultiPart::related().singlepart(SinglePart::html(email.render(opts)?)))
    }

    fn build_attachment(file: &MultipartFile) -> SinglePart {
        let body = Body::new(std::fs::read(file.filepath.clone()).unwrap());
        Attachment::new(file.filename.clone()).body(
            body,
            file.content_type
                .as_ref()
                .unwrap()
                .to_string()
                .parse()
                .unwrap(),
        )
    }

    fn get_multipart(
        &self,
        template: &Mjml,
        template_opts: &TemplateOptions,
        render_opts: &RenderOptions,
    ) -> Result<MultiPart, TemplateError> {
        let builder = MultiPart::mixed();
        let builder = builder.multipart(Self::get_body(template, render_opts)?);
        Ok(template_opts.attachments.iter().fold(builder, |res, item| {
            res.singlepart(Self::build_attachment(item))
        }))
    }

    pub fn to_email(
        &self,
        template_opts: &TemplateOptions,
        render_opts: &RenderOptions,
    ) -> Result<Message, TemplateError> {
        // debug!("rendering template: {} ({})", self.name, self.description);
        let email = self.render(template_opts)?;
        let builder = template_opts.to_builder();
        Ok(builder
            .subject(email.get_title().unwrap_or_default().as_str())
            .multipart(self.get_multipart(&email, template_opts, render_opts)?)?)
    }
}

// LCOV_EXCL_START
// #[cfg(test)]
// mod tests {
//     use super::{Template, TemplateOptions};
//     use crate::params::Config;
//     use mrml::prelude::render::Options as RenderOptions;
//     use serde_json::json;

//     #[test]
//     fn building_mrml_options_disable_comments_unset() {
//         let cfg = Config::from_args(vec![]);
//         let result = cfg.render_options();
//         assert_eq!(
//             result.disable_comments,
//             RenderOptions::default().disable_comments
//         );
//     }
//     #[test]
//     fn building_mrml_options_disable_comments_set() {
//         let cfg = Config::from_args(vec!["--mrml-disable-comments".to_string()]);
//         let result = cfg.render_options();
//         assert!(result.disable_comments);
//     }

//     #[test]
//     #[serial]
//     fn building_mrml_options_social_icon_origin_unset() {
//         let cfg = Config::from_args(vec![]);
//         let result = cfg.render_options();
//         assert_eq!(
//             result.social_icon_origin,
//             RenderOptions::default().social_icon_origin
//         );
//     }
//     #[test]
//     #[serial]
//     fn building_mrml_options_social_icon_origin_set() {
//         let cfg = Config::from_args(vec![
//             "--mrml-social-icon-origin".to_string(),
//             "http://wherever.com/".to_string(),
//         ]);
//         let result = cfg.render_options();
//         assert_eq!(
//             result.social_icon_origin,
//             Some("http://wherever.com/".to_string())
//         );
//     }

//     #[test]
//     fn render_success() {
//         let tmpl = Template {
//             name: "hello".into(),
//             description: "world".into(),
//             content: "<mjml></mjml>".into(),
//             attributes: json!({
//                 "type": "object",
//                 "properties": {
//                     "name": {
//                         "type": "string"
//                     }
//                 },
//             }),
//         };
//         let opts = TemplateOptions::new(
//             "sender@example.com".into(),
//             vec!["recipient@example.com".into()],
//             vec![],
//             vec![],
//             json!({"name": "Alice"}),
//             vec![],
//         );
//         let result = tmpl.render(&opts);
//         assert!(result.is_ok());
//     }

//     #[test]
//     fn to_email_success() {
//         let cfg = Config::from_args(vec![]);
//         let tmpl = Template {
//             name: "hello".into(),
//             description: "world".into(),
//             content: "<mjml></mjml>".into(),
//             attributes: json!({
//                 "type": "object",
//                 "properties": {
//                     "name": {
//                         "type": "string"
//                     }
//                 },
//             }),
//         };
//         let opts = TemplateOptions::new(
//             "sender@example.com".into(),
//             vec!["recipient@example.com".into()],
//             vec![],
//             vec![],
//             json!({"name": "Alice"}),
//             vec![],
//         );
//         let result = tmpl.to_email(&opts, &cfg.render_options());
//         assert!(result.is_ok());
//     }
// }
// LCOV_EXCL_END
