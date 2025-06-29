use crate::service::smtp::SmtpPool;
use axum::body::Bytes;
use axum::extract::multipart::Field;
use axum::extract::{Extension, FromRequest, Multipart, Path, Request};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use catapulte_engine::Attachment;
use lettre::message::{header::ContentType, Mailbox};
use lettre::AsyncTransport;
use utoipa::openapi::Type;

async fn field_to_bytes(field: Field<'_>) -> Result<Bytes, MultipartError> {
    field.bytes().await.map_err(MultipartError::FailedMultipart)
}

async fn field_to_string(field: Field<'_>) -> Result<String, MultipartError> {
    let bytes = field_to_bytes(field).await?;
    String::from_utf8(bytes.to_vec()).map_err(MultipartError::InvalidString)
}

async fn field_to_json_value(field: Field<'_>) -> Result<serde_json::Value, MultipartError> {
    let bytes = field_to_bytes(field).await?;
    serde_json::from_slice(&bytes).map_err(MultipartError::InvalidJson)
}

async fn field_to_file(field: Field<'_>) -> Result<Attachment, MultipartError> {
    let filename = field
        .file_name()
        .map(String::from)
        .ok_or(MultipartError::FilenameMissing)?;
    let content_type = field
        .content_type()
        .map(String::from)
        .ok_or(MultipartError::MissingContentType)?;
    let content_type =
        ContentType::parse(&content_type).map_err(MultipartError::InvalidContentType)?;

    let content = field
        .bytes()
        .await
        .map_err(MultipartError::FailedMultipart)?;
    let content = lettre::message::Body::new(content.into_iter().collect::<Vec<u8>>());
    Ok(Attachment {
        filename,
        content_type,
        content,
    })
}

#[derive(Debug)]
pub(crate) enum MultipartError {
    MissingFromField,
    MissingContentType,
    FailedMultipart(axum::extract::multipart::MultipartError),
    FilenameMissing,
    InvalidContentType(lettre::message::header::ContentTypeErr),
    InvalidString(std::string::FromUtf8Error),
    InvalidJson(serde_json::Error),
    InvalidMultipart(axum::extract::multipart::MultipartRejection),
    InvalidMailbox(&'static str, lettre::address::AddressError),
}

impl std::error::Error for MultipartError {}

impl std::fmt::Display for MultipartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingFromField => write!(f, "Missing from field"),
            Self::MissingContentType => write!(f, "Missing attachment content type"),
            Self::FailedMultipart(inner) => write!(f, "Failed Multipart {inner}"),
            Self::FilenameMissing => write!(f, "Filename missing for file"),
            Self::InvalidContentType(inner) => {
                write!(f, "Invalid attachment content file {inner}")
            }
            Self::InvalidJson(inner) => write!(f, "Invalid Json {inner}"),
            Self::InvalidString(inner) => write!(f, "Invalid String {inner}"),
            Self::InvalidMultipart(inner) => write!(f, "Invalid Multipart {inner}"),
            Self::InvalidMailbox(field, err) => {
                write!(f, "Invalid Mailbox for field {field}: {err}")
            }
        }
    }
}

impl IntoResponse for MultipartError {
    fn into_response(self) -> axum::response::Response {
        (StatusCode::BAD_REQUEST).into_response()
    }
}

impl<S> FromRequest<S> for MultipartPayload
where
    S: Send + Sync,
{
    type Rejection = MultipartError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let multipart = Multipart::from_request(req, state)
            .await
            .map_err(MultipartError::InvalidMultipart)?;
        let builder = MultipartPayloadBuilder::from_multipart(multipart).await?;
        builder.build()
    }
}

impl utoipa::ToSchema for MultipartPayload {
    fn name() -> std::borrow::Cow<'static, str> {
        "MultipartPayload".into()
    }
}

impl utoipa::PartialSchema for MultipartPayload {
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
        utoipa::openapi::ObjectBuilder::new()
            .property(
                "from",
                utoipa::openapi::ObjectBuilder::new()
                    .schema_type(utoipa::openapi::schema::SchemaType::Type(Type::String)),
            )
            .required("from")
            .property("to", super::Recipient::schema())
            .property("cc", super::Recipient::schema())
            .property("bcc", super::Recipient::schema())
            .property(
                "params",
                utoipa::openapi::ObjectBuilder::new()
                    .schema_type(utoipa::openapi::schema::SchemaType::AnyValue),
            )
            .property(
                "attachments",
                utoipa::openapi::ArrayBuilder::new().items(
                    utoipa::openapi::ObjectBuilder::new()
                        .schema_type(utoipa::openapi::schema::SchemaType::Type(Type::String))
                        .format(Some(utoipa::openapi::SchemaFormat::KnownFormat(
                            utoipa::openapi::KnownFormat::Binary,
                        ))),
                ),
            )
            .into()
    }
}

#[derive(Debug, Default)]
pub(crate) struct MultipartPayloadBuilder {
    from: Option<Mailbox>,
    to: Vec<Mailbox>,
    cc: Vec<Mailbox>,
    bcc: Vec<Mailbox>,
    params: Option<serde_json::Value>,
    attachments: Vec<Attachment>,
}

impl MultipartPayloadBuilder {
    fn build(self) -> Result<MultipartPayload, MultipartError> {
        let from = self.from.ok_or(MultipartError::MissingFromField)?;

        Ok(MultipartPayload {
            from,
            to: self.to,
            cc: self.cc,
            bcc: self.bcc,
            params: self
                .params
                .unwrap_or_else(|| serde_json::Value::Object(Default::default())),
            attachments: self.attachments,
        })
    }

    async fn parse_from<'a>(&mut self, field: Field<'a>) -> Result<(), MultipartError> {
        let from = field_to_string(field).await?;
        let from = from
            .parse::<Mailbox>()
            .map_err(|err| MultipartError::InvalidMailbox("from", err))?;
        self.from = Some(from);
        Ok(())
    }

    async fn parse_to<'a>(&mut self, field: Field<'a>) -> Result<(), MultipartError> {
        let address = field_to_string(field).await?;
        let address = address
            .parse::<Mailbox>()
            .map_err(|err| MultipartError::InvalidMailbox("to", err))?;
        self.to.push(address);
        Ok(())
    }

    async fn parse_cc<'a>(&mut self, field: Field<'a>) -> Result<(), MultipartError> {
        let address = field_to_string(field).await?;
        let address = address
            .parse::<Mailbox>()
            .map_err(|err| MultipartError::InvalidMailbox("cc", err))?;
        self.cc.push(address);
        Ok(())
    }

    async fn parse_bcc<'a>(&mut self, field: Field<'a>) -> Result<(), MultipartError> {
        let address = field_to_string(field).await?;
        let address = address
            .parse::<Mailbox>()
            .map_err(|err| MultipartError::InvalidMailbox("bcc", err))?;
        self.bcc.push(address);
        Ok(())
    }

    async fn parse_params<'a>(&mut self, field: Field<'a>) -> Result<(), MultipartError> {
        self.params = Some(field_to_json_value(field).await?);
        Ok(())
    }

    async fn parse_attachment<'a>(&mut self, field: Field<'a>) -> Result<(), MultipartError> {
        self.attachments.push(field_to_file(field).await?);
        Ok(())
    }

    async fn parse_field<'a>(&mut self, field: Field<'a>) -> Result<(), MultipartError> {
        match field.name() {
            Some("from") => self.parse_from(field).await?,
            Some("to") => self.parse_to(field).await?,
            Some("cc") => self.parse_cc(field).await?,
            Some("bcc") => self.parse_bcc(field).await?,
            Some("params") => self.parse_params(field).await?,
            Some("attachments") => self.parse_attachment(field).await?,
            _ => (),
        };
        Ok(())
    }

    async fn parse(&mut self, mut body: Multipart) -> Result<(), MultipartError> {
        while let Ok(Some(field)) = body.next_field().await {
            self.parse_field(field).await?;
        }
        Ok(())
    }

    async fn from_multipart(body: Multipart) -> Result<Self, MultipartError> {
        let mut res = Self::default();
        res.parse(body).await?;
        Ok(res)
    }
}

#[derive(Debug)]
pub(crate) struct MultipartPayload {
    from: Mailbox,
    to: Vec<Mailbox>,
    cc: Vec<Mailbox>,
    bcc: Vec<Mailbox>,
    params: serde_json::Value,
    attachments: Vec<Attachment>,
}

impl MultipartPayload {
    fn into_request(self, name: String) -> catapulte_engine::Request {
        catapulte_engine::Request {
            name,
            from: self.from,
            to: self.to,
            cc: self.cc,
            bcc: self.bcc,
            params: self.params,
            attachments: self.attachments,
        }
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
        (status = 204, description = "Your email has been sent.", body = ()),
    )
)]
pub(crate) async fn handler(
    Extension(smtp_pool): Extension<SmtpPool>,
    Extension(engine): Extension<catapulte_engine::Engine>,
    Path(name): Path<String>,
    body: MultipartPayload,
) -> Result<StatusCode, crate::error::ServerError> {
    metrics::counter!("smtp_send", "method" => "multipart", "template_name" => name.clone())
        .increment(1);

    let req = body.into_request(name.clone());
    let message = engine.handle(req).await?;
    if let Err(err) = smtp_pool.send(message).await {
        metrics::counter!("smtp_send_error", "method" => "multipart", "template_name" => name)
            .increment(1);
        Err(err)?
    } else {
        metrics::counter!("smtp_send_success", "method" => "multipart", "template_name" => name)
            .increment(1);
        Ok(StatusCode::NO_CONTENT)
    }
}

#[cfg(test)]
mod integration_tests {
    use crate::service::server::Server;
    use crate::service::smtp::tests::{
        create_email, smtp_image_insecure, SmtpMock, HTTP_PORT, SMTP_PORT,
    };
    use axum::body::Body;
    use axum::http::{Method, Request};
    use multipart::client::lazy::Multipart;
    use std::io::{BufReader, Read};
    use std::path::{Path, PathBuf};
    use testcontainers::runners::AsyncRunner;
    use tower::ServiceExt;

    fn build_request<'a>(
        name: &str,
        text: Vec<(&'static str, &'a str)>,
        files: Vec<&'a Path>,
    ) -> Request<Body> {
        let mut body = Multipart::new();
        for (key, value) in text {
            body.add_text(key, value);
        }
        for path in files {
            body.add_file("attachments", path);
        }

        let prepared = body.prepare().unwrap();
        let content_len = prepared.content_len();
        let boundary = prepared.boundary().to_owned();

        let content_type = multipart::client::hyper::content_type(&boundary);

        let mut buffer = Vec::new();
        BufReader::new(prepared).read_to_end(&mut buffer).unwrap();
        let compatible_body = axum::body::Body::from(buffer);

        let uri = format!("/templates/{name}/multipart");
        Request::builder()
            .method(Method::POST)
            .uri(uri)
            .header(axum::http::header::CONTENT_TYPE, content_type.to_string())
            .header(axum::http::header::CONTENT_LENGTH, content_len.unwrap())
            .body(compatible_body)
            .unwrap()
    }

    #[tokio::test]
    async fn success_without_attachment() {
        crate::try_init_logs();

        let smtp_node = smtp_image_insecure().start().await.unwrap();
        let smtp_port = smtp_node.get_host_port_ipv4(SMTP_PORT).await.unwrap();
        let http_port = smtp_node.get_host_port_ipv4(HTTP_PORT).await.unwrap();

        let smtp_mock = SmtpMock::new("localhost", http_port);

        let app = Server::default_insecure(smtp_port).app();
        //
        let from = create_email();
        let to = create_email();
        //
        let req = build_request(
            "user-login",
            vec![
                ("from", &from.to_string()),
                ("to", &to.to_string()),
                ("params", r#"{"name":"bob","token":"token"}"#),
            ],
            Vec::new(),
        );
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), axum::http::StatusCode::NO_CONTENT);
        //
        let messages = smtp_mock.expect_latest_inbox().await;
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].inner.subject, "Hello bob!");
        let msg = messages[0].detailed().await;
        let text = msg.plaintext().await;
        assert!(text.contains("Hello bob!"));
        let html = msg.html().await;
        assert!(html.contains("Hello bob!"));
        assert!(html.contains("\"http://example.com/login?token=token\""));
    }

    #[tokio::test]
    async fn success_with_attachment() {
        crate::try_init_logs();

        let smtp_node = smtp_image_insecure().start().await.unwrap();
        let smtp_port = smtp_node.get_host_port_ipv4(SMTP_PORT).await.unwrap();
        let http_port = smtp_node.get_host_port_ipv4(HTTP_PORT).await.unwrap();

        let smtp_mock = SmtpMock::new("localhost", http_port);

        let app = Server::default_insecure(smtp_port).app();
        //
        let from = create_email();
        let to = create_email();
        let cat = PathBuf::new().join("asset").join("cat.jpg");
        //
        let req = build_request(
            "user-login",
            vec![
                ("from", &from.to_string()),
                ("to", &to.to_string()),
                ("params", r#"{"name":"bob","token":"token"}"#),
            ],
            vec![&cat],
        );
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), axum::http::StatusCode::NO_CONTENT);
        //
        let messages = smtp_mock.expect_latest_inbox().await;
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].inner.subject, "Hello bob!");
        let msg = messages[0].detailed().await;
        let text = msg.plaintext().await;
        assert!(text.contains("Hello bob!"));
        let html = msg.html().await;
        assert!(html.contains("Hello bob!"));
        assert!(html.contains("\"http://example.com/login?token=token\""));
    }

    #[tokio::test]
    async fn success_multiple_recipients() {
        crate::try_init_logs();

        let smtp_node = smtp_image_insecure().start().await.unwrap();
        let smtp_port = smtp_node.get_host_port_ipv4(SMTP_PORT).await.unwrap();
        let http_port = smtp_node.get_host_port_ipv4(HTTP_PORT).await.unwrap();

        let smtp_mock = SmtpMock::new("localhost", http_port);

        let app = Server::default_insecure(smtp_port).app();
        //
        let from = create_email();
        let to_first = create_email();
        let to_second = create_email();
        let cc = create_email();
        //
        let req = build_request(
            "user-login",
            vec![
                ("from", &from.to_string()),
                ("to", &to_first.to_string()),
                ("to", &to_second.to_string()),
                ("cc", &cc.to_string()),
                ("params", r#"{"name":"bob","token":"token"}"#),
            ],
            Vec::new(),
        );
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), axum::http::StatusCode::NO_CONTENT);
        //
        let messages = smtp_mock.expect_latest_inbox().await;
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].inner.subject, "Hello bob!");
        let msg = messages[0].detailed().await;
        let text = msg.plaintext().await;
        assert!(text.contains("Hello bob!"));
        let html = msg.html().await;
        assert!(html.contains("Hello bob!"));
        assert!(html.contains("\"http://example.com/login?token=token\""));
        // expect_latest_inbox(&from, "to", &to_second).await;
        // expect_latest_inbox(&from, "cc", &cc).await;
    }
}
