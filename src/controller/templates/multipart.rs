use crate::error::ServerError;
use crate::service::multipart::{
    field_to_file, field_to_json_value, field_to_string, MultipartFile,
};
use crate::service::provider::TemplateProvider;
use crate::service::smtp::SmtpPool;
use crate::service::template::TemplateOptions;
use axum::extract::multipart::Field;
use axum::extract::{Extension, Multipart, Path};
use axum::http::StatusCode;
use lettre::Transport;
use mrml::prelude::render::Options as RenderOptions;
use serde_json::Value as JsonValue;
use std::default::Default;
use std::sync::Arc;
use tempfile::TempDir;

#[derive(Default)]
pub(crate) struct MultipartPayload {
    from: String,
    to: Vec<String>,
    cc: Vec<String>,
    bcc: Vec<String>,
    params: Option<JsonValue>,
    attachments: Vec<MultipartFile>,
}

impl utoipa::ToSchema for MultipartPayload {
    fn schema() -> utoipa::openapi::schema::Schema {
        utoipa::openapi::ObjectBuilder::new()
            .property(
                "from",
                utoipa::openapi::ObjectBuilder::new()
                    .schema_type(utoipa::openapi::SchemaType::String),
            )
            .required("from")
            .property("to", super::json::Recipient::schema())
            .property("cc", super::json::Recipient::schema())
            .property("bcc", super::json::Recipient::schema())
            .property(
                "params",
                utoipa::openapi::ObjectBuilder::new()
                    .schema_type(utoipa::openapi::SchemaType::Object),
            )
            .property(
                "attachments",
                utoipa::openapi::ArrayBuilder::new().items(
                    utoipa::openapi::ObjectBuilder::new()
                        .schema_type(utoipa::openapi::SchemaType::String)
                        .format(Some(utoipa::openapi::SchemaFormat::KnownFormat(
                            utoipa::openapi::KnownFormat::Binary,
                        ))),
                ),
            )
            .into()
    }
}

impl MultipartPayload {
    async fn parse_from<'a>(&mut self, field: Field<'a>) -> Result<(), ServerError> {
        if let Ok(from) = field_to_string(field).await {
            self.from = from;
        }
        Ok(())
    }

    async fn parse_to<'a>(&mut self, field: Field<'a>) -> Result<(), ServerError> {
        if let Ok(address) = field_to_string(field).await {
            self.to.push(address);
        }
        Ok(())
    }

    async fn parse_cc<'a>(&mut self, field: Field<'a>) -> Result<(), ServerError> {
        if let Ok(address) = field_to_string(field).await {
            self.cc.push(address);
        }
        Ok(())
    }

    async fn parse_bcc<'a>(&mut self, field: Field<'a>) -> Result<(), ServerError> {
        if let Ok(address) = field_to_string(field).await {
            self.bcc.push(address);
        }
        Ok(())
    }

    async fn parse_params<'a>(&mut self, field: Field<'a>) -> Result<(), ServerError> {
        self.params = field_to_json_value(field).await.ok();
        Ok(())
    }

    async fn parse_attachment<'a>(
        &mut self,
        root: &std::path::Path,
        field: Field<'a>,
    ) -> Result<(), ServerError> {
        match field_to_file(root, field).await {
            Ok(file) => {
                self.attachments.push(file);
                Ok(())
            }
            Err(err) => Err(ServerError::bad_request(err.to_string())),
        }
    }

    async fn parse_field<'a>(
        &mut self,
        root: &std::path::Path,
        field: Field<'a>,
    ) -> Result<(), ServerError> {
        match field.name() {
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

    async fn parse(
        &mut self,
        root: &std::path::Path,
        mut body: Multipart,
    ) -> Result<(), ServerError> {
        while let Ok(Some(field)) = body.next_field().await {
            self.parse_field(root, field).await?;
        }
        Ok(())
    }

    pub async fn from_multipart(
        root: &std::path::Path,
        body: Multipart,
    ) -> Result<Self, ServerError> {
        let mut res = Self::default();
        res.parse(root, body).await?;
        Ok(res)
    }
}

impl From<MultipartPayload> for TemplateOptions {
    fn from(value: MultipartPayload) -> Self {
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

#[utoipa::path(
    operation_id = "send_multipart",
    post,
    path = "/templates/{name}/multipart",
    params(
        ("name" = String, Path, description = "Name of the template.")
    ),
    request_body(
        content = MultipartPayload,
        content_type = "multipart/form-data",
    ),
    responses(
        (status = 204, description = "Your email has been sent.", body = None),
    )
)]
pub(crate) async fn handler(
    Extension(render_opts): Extension<Arc<RenderOptions>>,
    Extension(smtp_pool): Extension<SmtpPool>,
    Extension(template_provider): Extension<Arc<TemplateProvider>>,
    Path(name): Path<String>,
    body: Multipart,
) -> Result<StatusCode, ServerError> {
    metrics::increment_counter!("smtp_send", "method" => "multipart", "template_name" => name.clone());
    let template = template_provider.find_by_name(name.as_str()).await?;
    let tmp_dir = TempDir::new()?;
    let tmp_path = tmp_dir.path().to_owned();
    let parser = MultipartPayload::from_multipart(&tmp_path, body).await?;
    let options: TemplateOptions = parser.into();
    options.validate()?;
    let email = template.to_email(&options, render_opts.as_ref())?;
    if let Err(err) = smtp_pool.send(&email) {
        metrics::increment_counter!("smtp_send_error", "method" => "multipart", "template_name" => name);
        Err(err)?
    } else {
        metrics::increment_counter!("smtp_send_success", "method" => "multipart", "template_name" => name);
        Ok(StatusCode::NO_CONTENT)
    }
}

// #[cfg(test)]
// mod tests {
// use super::handler;
// use crate::tests::{create_email, get_latest_inbox};
// use axum::extract::{Extension, Path};
// use axum::http::StatusCode;
// use bytes::{BufMut, Bytes, BytesMut};
// use common_multipart_rfc7578 as cmultipart;
// use futures::TryStreamExt;
// use serde_json::json;
// use std::fs::File;
// use std::io::BufReader;
// use std::sync::Arc;

// async fn to_bytes(form: cmultipart::client::multipart::Form<'_>) -> Bytes {
//     let mut body = cmultipart::client::multipart::Body::from(form);
//     let mut bytes = BytesMut::new();
//     while let Ok(Some(field)) = body.try_next().await {
//         bytes.put(field.to_vec().as_slice());
//     }
//     bytes.into()
// }

// #[tokio::test]
// #[serial_test::serial]
// async fn success_with_file() {
//     let render_options = Arc::new(crate::service::render::Configuration::default().build());
//     let smtp_pool = crate::service::smtp::Configuration::insecure()
//         .build()
//         .unwrap();
//     let template_provider =
//         Arc::new(crate::service::provider::Configuration::default().build());

//     let from = create_email();
//     let to = create_email();

//     let payload = json!({
//         "name": "bob",
//         "token": "this_is_a_token"
//     });
//     let file = File::open("asset/cat.jpg").unwrap();
//     let reader = BufReader::new(file);
//     let mut form = cmultipart::client::multipart::Form::default();
//     form.add_text("from", from.clone());
//     form.add_text("to", to.clone());
//     form.add_text("params", payload.to_string());
//     form.add_reader_file("attachments", reader, "cat.jpg");
//     let content_type = form.content_type();
//     let bytes = to_bytes(form).await;
//     // let payload = Multipart::

//     let result = handler(
//         Extension(render_options),
//         Extension(smtp_pool),
//         Extension(template_provider),
//         Path("user-login".into()),
//         payload,
//     )
//     .await
//     .unwrap();

//     assert_eq!(result, StatusCode::NO_CONTENT);
//     let list = get_latest_inbox(&from, &to).await;
//     assert!(!list.is_empty());
//     let last = list.first().unwrap();
//     assert!(last.text.contains("Hello bob!"));
//     assert!(last.html.contains("Hello bob!"));
//     assert!(last
//         .html
//         .contains("\"http://example.com/login?token=this_is_a_token\""));
// }

//     #[actix_rt::test]
//     #[serial]
//     async fn error_with_file_without_filename() {
//         let from = create_email();
//         let to = create_email();
//         let payload = json!({
//             "name": "bob",
//             "token": "this_is_a_token"
//         });
//         let file = File::open("asset/cat.jpg").unwrap();
//         let reader = BufReader::new(file);
//         let mut form = cmultipart::client::multipart::Form::default();
//         form.add_text("from", from.clone());
//         form.add_text("to", to.clone());
//         form.add_text("params", payload.to_string());
//         form.add_reader("attachments", reader);
//         let content_type = form.content_type();
//         let bytes = to_bytes(form).await;
//         let req = test::TestRequest::post()
//             .insert_header(("content-type", content_type))
//             .uri("/templates/user-login")
//             .set_payload(bytes)
//             .to_request();
//         let res = ServerBuilder::default().execute(req).await;
//         assert_eq!(res.status(), StatusCode::BAD_REQUEST);
//     }

//     #[actix_rt::test]
//     #[serial]
//     async fn success_with_multiple_recipients() {
//         let from = create_email();
//         let to_first = create_email();
//         let to_second = create_email();
//         let payload = json!({
//             "name": "bob",
//             "token": "this_is_a_token"
//         });
//         let file = File::open("asset/cat.jpg").unwrap();
//         let reader = BufReader::new(file);
//         let mut form = cmultipart::client::multipart::Form::default();
//         form.add_text("from", from.clone());
//         form.add_text("to", to_first.clone());
//         form.add_text("to", to_second.clone());
//         form.add_text("cc", create_email());
//         form.add_text("bcc", create_email());
//         form.add_text("params", payload.to_string());
//         form.add_reader_file("attachments", reader, "cat.jpg");
//         let content_type = form.content_type();
//         let bytes = to_bytes(form).await;
//         let req = test::TestRequest::post()
//             .insert_header(("content-type", content_type))
//             .uri("/templates/user-login")
//             .set_payload(bytes)
//             .to_request();
//         let res = ServerBuilder::default().execute(req).await;
//         assert_eq!(res.status(), StatusCode::NO_CONTENT);
//         let list = get_latest_inbox(&from, &to_first).await;
//         assert!(!list.is_empty());
//         let last = list.first().unwrap();
//         assert!(last.text.contains("Hello bob!"));
//         assert!(last.html.contains("Hello bob!"));
//         assert!(last
//             .html
//             .contains("\"http://example.com/login?token=this_is_a_token\""));
//     }
// }
