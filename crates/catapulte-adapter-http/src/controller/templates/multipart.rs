use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::multipart::Field;
use axum::extract::{Extension, FromRequest, Multipart, Path, Request};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use catapulte_domain::model::{Attachment, Email, Recipient as DomainRecipient, Recipients};
use catapulte_domain::prelude::{EmailSender, TemplateLoader, TemplateRenderer};
use catapulte_domain::service::SendEmailService;
use utoipa::openapi::Type;

use super::Recipient;
use crate::error::ErrorResponse;

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
        .unwrap_or_else(|| "application/octet-stream".to_string());
    let content = field
        .bytes()
        .await
        .map_err(MultipartError::FailedMultipart)?;

    Ok(Attachment {
        filename,
        content_type,
        content: content.to_vec(),
    })
}

#[derive(Debug)]
pub enum MultipartError {
    MissingFromField,
    FailedMultipart(axum::extract::multipart::MultipartError),
    FilenameMissing,
    InvalidString(std::string::FromUtf8Error),
    InvalidJson(serde_json::Error),
    InvalidMultipart(axum::extract::multipart::MultipartRejection),
    InvalidMailbox(String),
}

impl std::error::Error for MultipartError {}

impl std::fmt::Display for MultipartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingFromField => write!(f, "Missing from field"),
            Self::FailedMultipart(inner) => write!(f, "Failed Multipart {inner}"),
            Self::FilenameMissing => write!(f, "Filename missing for file"),
            Self::InvalidJson(inner) => write!(f, "Invalid Json {inner}"),
            Self::InvalidString(inner) => write!(f, "Invalid String {inner}"),
            Self::InvalidMultipart(inner) => write!(f, "Invalid Multipart {inner}"),
            Self::InvalidMailbox(msg) => write!(f, "Invalid Mailbox: {msg}"),
        }
    }
}

impl IntoResponse for MultipartError {
    fn into_response(self) -> axum::response::Response {
        let response = ErrorResponse {
            status: StatusCode::BAD_REQUEST,
            code: "invalid-multipart",
            title: "invalid multipart request",
            details: vec![self.to_string()],
        };
        response.into_response()
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
            .property("to", Recipient::schema())
            .property("cc", Recipient::schema())
            .property("bcc", Recipient::schema())
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
struct MultipartPayloadBuilder {
    from: Option<DomainRecipient>,
    to: Vec<DomainRecipient>,
    cc: Vec<DomainRecipient>,
    bcc: Vec<DomainRecipient>,
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

    fn parse_mailbox(value: &str) -> Result<DomainRecipient, MultipartError> {
        // Try to parse "Name <email@example.com>" format
        if let Some(start) = value.find('<')
            && let Some(end) = value.find('>')
        {
            let name = value[..start].trim();
            let email = value[start + 1..end].trim();
            if !email.contains('@') {
                return Err(MultipartError::InvalidMailbox(format!(
                    "invalid email: {email}"
                )));
            }
            return Ok(DomainRecipient {
                name: if name.is_empty() {
                    None
                } else {
                    Some(name.to_string())
                },
                email: email.to_string(),
            });
        }

        // Plain email address
        if !value.contains('@') {
            return Err(MultipartError::InvalidMailbox(format!(
                "invalid email: {value}"
            )));
        }
        Ok(DomainRecipient {
            name: None,
            email: value.to_string(),
        })
    }

    async fn parse_from(&mut self, field: Field<'_>) -> Result<(), MultipartError> {
        let from = field_to_string(field).await?;
        self.from = Some(Self::parse_mailbox(&from)?);
        Ok(())
    }

    async fn parse_to(&mut self, field: Field<'_>) -> Result<(), MultipartError> {
        let address = field_to_string(field).await?;
        self.to.push(Self::parse_mailbox(&address)?);
        Ok(())
    }

    async fn parse_cc(&mut self, field: Field<'_>) -> Result<(), MultipartError> {
        let address = field_to_string(field).await?;
        self.cc.push(Self::parse_mailbox(&address)?);
        Ok(())
    }

    async fn parse_bcc(&mut self, field: Field<'_>) -> Result<(), MultipartError> {
        let address = field_to_string(field).await?;
        self.bcc.push(Self::parse_mailbox(&address)?);
        Ok(())
    }

    async fn parse_params(&mut self, field: Field<'_>) -> Result<(), MultipartError> {
        self.params = Some(field_to_json_value(field).await?);
        Ok(())
    }

    async fn parse_attachment(&mut self, field: Field<'_>) -> Result<(), MultipartError> {
        self.attachments.push(field_to_file(field).await?);
        Ok(())
    }

    async fn parse_field(&mut self, field: Field<'_>) -> Result<(), MultipartError> {
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
pub struct MultipartPayload {
    from: DomainRecipient,
    to: Vec<DomainRecipient>,
    cc: Vec<DomainRecipient>,
    bcc: Vec<DomainRecipient>,
    params: serde_json::Value,
    attachments: Vec<Attachment>,
}

impl MultipartPayload {
    fn into_email(self, template_name: String) -> Email {
        Email {
            template_name,
            from: self.from,
            recipients: Recipients {
                to: self.to,
                cc: self.cc,
                bcc: self.bcc,
            },
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
pub async fn handler<L, R, S>(
    Extension(service): Extension<Arc<SendEmailService<L, R, S>>>,
    Path(name): Path<String>,
    body: MultipartPayload,
) -> Result<StatusCode, ErrorResponse>
where
    L: TemplateLoader + 'static,
    R: TemplateRenderer + 'static,
    S: EmailSender + 'static,
{
    metrics::counter!("smtp_send", "method" => "multipart", "template_name" => name.clone())
        .increment(1);

    let email = body.into_email(name.clone());

    match service.send(&email).await {
        Ok(()) => {
            metrics::counter!("smtp_send_success", "method" => "multipart", "template_name" => name)
                .increment(1);
            Ok(StatusCode::NO_CONTENT)
        }
        Err(err) => {
            metrics::counter!("smtp_send_error", "method" => "multipart", "template_name" => name)
                .increment(1);
            Err(err.into())
        }
    }
}
