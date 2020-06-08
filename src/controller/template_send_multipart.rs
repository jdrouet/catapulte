use crate::error::ServerError;
use crate::service::multipart::{
    field_to_file, field_to_json_value, field_to_string, MultipartFile,
};
use crate::service::smtp::SmtpPool;
use crate::service::template::manager::TemplateManager;
use crate::service::template::provider::TemplateProvider;
use crate::service::template::template::TemplateOptions;
use actix_http::RequestHead;
use actix_multipart::{Field, Multipart};
use actix_web::{web, HttpResponse};
use futures::TryStreamExt;
use lettre::Transport;
use serde_json::Value as JsonValue;
use std::convert::TryInto;
use std::default::Default;
use std::path::Path;
use tempfile::TempDir;

pub fn filter(req: &RequestHead) -> bool {
    req.headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| Some(value.starts_with("multipart/form-data")))
        .unwrap_or(false)
}

#[derive(Default)]
struct TemplateOptionsParser {
    from: Option<String>,
    to: Option<String>,
    params: Option<JsonValue>,
    attachments: Vec<MultipartFile>,
}

impl TemplateOptionsParser {
    async fn parse_from(&mut self, field: Field) -> Result<(), ServerError> {
        self.from = field_to_string(field).await.ok();
        Ok(())
    }

    async fn parse_to(&mut self, field: Field) -> Result<(), ServerError> {
        self.to = field_to_string(field).await.ok();
        Ok(())
    }

    async fn parse_params(&mut self, field: Field) -> Result<(), ServerError> {
        self.params = field_to_json_value(field).await.ok();
        Ok(())
    }

    async fn parse_attachment(&mut self, root: &Path, field: Field) -> Result<(), ServerError> {
        if let Some(file) = field_to_file(root, field).await.ok() {
            self.attachments.push(file);
        }
        Ok(())
    }

    async fn parse_field(&mut self, root: &Path, field: Field) -> Result<(), ServerError> {
        let content = match field.content_disposition() {
            Some(value) => value,
            None => return Ok(()),
        };
        let field_name = match content.get_name() {
            Some(name) => name,
            None => return Ok(()),
        };
        match field_name {
            "from" => self.parse_from(field).await?,
            "to" => self.parse_to(field).await?,
            "params" => self.parse_params(field).await?,
            "attachments" => self.parse_attachment(root, field).await?,
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

impl TryInto<TemplateOptions> for TemplateOptionsParser {
    type Error = ServerError;

    fn try_into(self) -> Result<TemplateOptions, Self::Error> {
        if self.from.is_none() {
            Err(ServerError::BadRequest("missing field \"from\"".into()))
        } else if self.to.is_none() {
            Err(ServerError::BadRequest("missing field \"to\"".into()))
        } else if self.params.is_none() {
            Err(ServerError::BadRequest("missing field \"params\"".into()))
        } else {
            Ok(TemplateOptions::new(
                self.from.unwrap(),
                self.to.unwrap(),
                self.params.unwrap(),
                self.attachments,
            ))
        }
    }
}

// #[post("/templates/{name}/multipart")]
pub async fn handler(
    smtp_pool: web::Data<SmtpPool>,
    template_provider: web::Data<TemplateProvider>,
    name: web::Path<String>,
    body: Multipart,
) -> Result<HttpResponse, ServerError> {
    let template = template_provider.find_by_name(name.as_str())?;
    let tmp_dir = TempDir::new()?;
    let tmp_path = tmp_dir.path().to_owned();
    let parser = TemplateOptionsParser::from_multipart(&tmp_path, body).await?;
    let options: TemplateOptions = parser.try_into()?;
    let email = template.to_email(&options)?;
    let mut conn = smtp_pool.get()?;
    conn.send(email)?;
    Ok(HttpResponse::NoContent().finish())
}

#[cfg(test)]
mod tests {
    use crate::tests::{create_email, execute_request, get_latest_inbox};
    use actix_web::http::{Method, StatusCode};
    use actix_web::test;
    use actix_web::web::BytesMut;
    use bytes::buf::BufMut;
    use common_multipart_rfc7578 as cmultipart;
    use futures::TryStreamExt;
    use serde_json::json;
    use std::fs::File;
    use std::io::BufReader;

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
        let mut body = cmultipart::client::multipart::Body::from(form);
        let mut bytes = BytesMut::new();
        while let Ok(Some(field)) = body.try_next().await {
            bytes.put(field);
        }
        let req = test::TestRequest::with_header("content-type", content_type)
            .method(Method::POST)
            .uri("/templates/user-login")
            .set_payload(bytes)
            .to_request();
        let res = execute_request(req).await;
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
        let list = get_latest_inbox(&from, &to).await;
        assert!(list.len() > 0);
        let last = list.first().unwrap();
        assert!(last.text.contains("Hello bob!"));
        assert!(last.html.contains("Hello bob!"));
        assert!(last
            .html
            .contains("\"http://example.com/login?token=this_is_a_token\""));
    }
}
