use crate::error::ServerError;
use crate::service::multipart::{
    field_to_file, field_to_json_value, field_to_string, MultipartFile,
};
use crate::service::provider::TemplateProvider;
use crate::service::smtp::SmtpPool;
use crate::service::template::TemplateOptions;
use actix_multipart::{Field, Multipart};
use actix_web::{guard::GuardContext, web, HttpResponse};
use futures::TryStreamExt;
use lettre::Transport;
use mrml::prelude::render::Options as RenderOptions;
use serde_json::Value as JsonValue;
use std::default::Default;
use std::path::Path;
use tempfile::TempDir;

pub fn filter(ctx: &GuardContext) -> bool {
    ctx.head()
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .map(|value| value.starts_with("multipart/form-data"))
        .unwrap_or(false)
}

#[derive(Default)]
struct TemplateOptionsParser {
    from: String,
    to: Vec<String>,
    cc: Vec<String>,
    bcc: Vec<String>,
    params: Option<JsonValue>,
    attachments: Vec<MultipartFile>,
}

impl TemplateOptionsParser {
    async fn parse_from(&mut self, field: Field) -> Result<(), ServerError> {
        if let Ok(from) = field_to_string(field).await {
            self.from = from;
        }
        Ok(())
    }

    async fn parse_to(&mut self, field: Field) -> Result<(), ServerError> {
        if let Ok(address) = field_to_string(field).await {
            self.to.push(address);
        }
        Ok(())
    }

    async fn parse_cc(&mut self, field: Field) -> Result<(), ServerError> {
        if let Ok(address) = field_to_string(field).await {
            self.cc.push(address);
        }
        Ok(())
    }

    async fn parse_bcc(&mut self, field: Field) -> Result<(), ServerError> {
        if let Ok(address) = field_to_string(field).await {
            self.bcc.push(address);
        }
        Ok(())
    }

    async fn parse_params(&mut self, field: Field) -> Result<(), ServerError> {
        self.params = field_to_json_value(field).await.ok();
        Ok(())
    }

    async fn parse_attachment(&mut self, root: &Path, field: Field) -> Result<(), ServerError> {
        match field_to_file(root, field).await {
            Ok(file) => {
                self.attachments.push(file);
                Ok(())
            }
            Err(err) => Err(ServerError::BadRequest(err.to_string())),
        }
    }

    async fn parse_field(&mut self, root: &Path, field: Field) -> Result<(), ServerError> {
        let content = field.content_disposition();
        let field_name = content.get_name();
        match field_name {
            Some("from") => self.parse_from(field).await?,
            Some("to") => self.parse_to(field).await?,
            Some("cc") => self.parse_cc(field).await?,
            Some("bcc") => self.parse_bcc(field).await?,
            Some("params") => self.parse_params(field).await?,
            Some("attachments") => self.parse_attachment(root, field).await?,
            _ => (),
        };
        Ok(())
    }

    async fn parse(&mut self, root: &Path, mut body: Multipart) -> Result<(), ServerError> {
        while let Ok(Some(field)) = body.try_next().await {
            self.parse_field(root, field).await?;
        }
        Ok(())
    }

    pub async fn from_multipart(root: &Path, body: Multipart) -> Result<Self, ServerError> {
        let mut res = Self::default();
        res.parse(root, body).await?;
        Ok(res)
    }
}

impl From<TemplateOptionsParser> for TemplateOptions {
    fn from(value: TemplateOptionsParser) -> Self {
        Self::new(
            value.from,
            value.to,
            value.cc,
            value.bcc,
            value.params.unwrap_or_default(),
            value.attachments,
        )
    }
}

pub async fn handler(
    render_opts: web::Data<RenderOptions>,
    smtp_pool: web::Data<SmtpPool>,
    template_provider: web::Data<TemplateProvider>,
    name: web::Path<String>,
    body: Multipart,
) -> Result<HttpResponse, ServerError> {
    let template = template_provider.find_by_name(name.as_str()).await?;
    let tmp_dir = TempDir::new()?;
    let tmp_path = tmp_dir.path().to_owned();
    let parser = TemplateOptionsParser::from_multipart(&tmp_path, body).await?;
    let options: TemplateOptions = parser.into();
    options.validate()?;
    let email = template.to_email(&options, render_opts.as_ref())?;
    smtp_pool.send(&email)?;
    Ok(HttpResponse::NoContent().finish())
}

// LCOV_EXCL_START
#[cfg(test)]
mod tests {
    use crate::tests::{create_email, get_latest_inbox, ServerBuilder};
    use actix_web::http::StatusCode;
    use actix_web::test;
    use actix_web::web::{BufMut, Bytes, BytesMut};
    use common_multipart_rfc7578 as cmultipart;
    use futures::TryStreamExt;
    use serde_json::json;
    use std::fs::File;
    use std::io::BufReader;

    async fn to_bytes(form: cmultipart::client::multipart::Form<'_>) -> Bytes {
        let mut body = cmultipart::client::multipart::Body::from(form);
        let mut bytes = BytesMut::new();
        while let Ok(Some(field)) = body.try_next().await {
            bytes.put(field.to_vec().as_slice());
        }
        bytes.into()
    }

    #[actix_rt::test]
    #[serial]
    async fn success_with_file() {
        let from = create_email();
        let to = create_email();
        let payload = json!({
            "name": "bob",
            "token": "this_is_a_token"
        });
        let file = File::open("asset/cat.jpg").unwrap();
        let reader = BufReader::new(file);
        let mut form = cmultipart::client::multipart::Form::default();
        form.add_text("from", from.clone());
        form.add_text("to", to.clone());
        form.add_text("params", payload.to_string());
        form.add_reader_file("attachments", reader, "cat.jpg");
        let content_type = form.content_type();
        let bytes = to_bytes(form).await;
        let req = test::TestRequest::post()
            .insert_header(("content-type", content_type))
            .uri("/templates/user-login")
            .set_payload(bytes)
            .to_request();
        let res = ServerBuilder::default().execute(req).await;
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
        let list = get_latest_inbox(&from, &to).await;
        assert!(!list.is_empty());
        let last = list.first().unwrap();
        assert!(last.text.contains("Hello bob!"));
        assert!(last.html.contains("Hello bob!"));
        assert!(last
            .html
            .contains("\"http://example.com/login?token=this_is_a_token\""));
    }

    #[actix_rt::test]
    #[serial]
    async fn error_with_file_without_filename() {
        let from = create_email();
        let to = create_email();
        let payload = json!({
            "name": "bob",
            "token": "this_is_a_token"
        });
        let file = File::open("asset/cat.jpg").unwrap();
        let reader = BufReader::new(file);
        let mut form = cmultipart::client::multipart::Form::default();
        form.add_text("from", from.clone());
        form.add_text("to", to.clone());
        form.add_text("params", payload.to_string());
        form.add_reader("attachments", reader);
        let content_type = form.content_type();
        let bytes = to_bytes(form).await;
        let req = test::TestRequest::post()
            .insert_header(("content-type", content_type))
            .uri("/templates/user-login")
            .set_payload(bytes)
            .to_request();
        let res = ServerBuilder::default().execute(req).await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }

    #[actix_rt::test]
    #[serial]
    async fn success_with_multiple_recipients() {
        let from = create_email();
        let to_first = create_email();
        let to_second = create_email();
        let payload = json!({
            "name": "bob",
            "token": "this_is_a_token"
        });
        let file = File::open("asset/cat.jpg").unwrap();
        let reader = BufReader::new(file);
        let mut form = cmultipart::client::multipart::Form::default();
        form.add_text("from", from.clone());
        form.add_text("to", to_first.clone());
        form.add_text("to", to_second.clone());
        form.add_text("cc", create_email());
        form.add_text("bcc", create_email());
        form.add_text("params", payload.to_string());
        form.add_reader_file("attachments", reader, "cat.jpg");
        let content_type = form.content_type();
        let bytes = to_bytes(form).await;
        let req = test::TestRequest::post()
            .insert_header(("content-type", content_type))
            .uri("/templates/user-login")
            .set_payload(bytes)
            .to_request();
        let res = ServerBuilder::default().execute(req).await;
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
        let list = get_latest_inbox(&from, &to_first).await;
        assert!(!list.is_empty());
        let last = list.first().unwrap();
        assert!(last.text.contains("Hello bob!"));
        assert!(last.html.contains("Hello bob!"));
        assert!(last
            .html
            .contains("\"http://example.com/login?token=this_is_a_token\""));
    }
}
// LCOV_EXCL_END
