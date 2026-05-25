use anyhow::Context;
use base64::Engine;
use catapulte_domain::entity::body::{BodySource, InvalidPlainBody, MjmlSource, Plain};
use catapulte_domain::entity::email::{EmailId, RecipientKind};
use catapulte_domain::use_case::submit_email::{AttachmentInput, SubmitEmailInput};
use serde::ser::SerializeStruct;
use serde::{Deserialize, Serialize, Serializer};

pub const MAX_ATTACHMENTS_PER_EMAIL: usize = 10;
pub const MAX_ATTACHMENT_BYTES: u64 = 25 * 1024 * 1024; // 25 MiB
// Worst case: 10 attachments × 25 MiB = 250 MiB binary, base64-inflated to ~333 MiB, plus ~1 MiB envelope.
pub const MAX_REQUEST_BODY_BYTES: usize = 352 * 1024 * 1024; // 352 MiB

#[derive(Debug, Deserialize)]
pub struct AttachmentDto {
    pub filename: String,
    pub content_type: String,
    #[serde(default)]
    pub inline_base64: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SubmitEmailRequest {
    #[serde(default)]
    pub idempotency_key: Option<String>,
    #[serde(default)]
    pub subject: Option<String>,
    pub sender: String,
    pub recipients: Vec<RecipientDto>,
    pub body: BodyDto,
    #[serde(default)]
    pub variables: serde_json::Map<String, serde_json::Value>,
    #[serde(default)]
    pub attachments: Vec<AttachmentDto>,
}

#[derive(Debug, Deserialize)]
pub struct RecipientDto {
    pub kind: RecipientKindDto,
    pub address: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecipientKindDto {
    To,
    Cc,
    Bcc,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BodyDto {
    Plain {
        text: Option<String>,
        html: Option<String>,
    },
    MjmlInline {
        source: String,
    },
    MjmlNamed {
        name: String,
    },
    MjmlRemote {
        url: String,
    },
}

pub struct SubmitEmailResponse {
    pub id: EmailId,
}

impl Serialize for SubmitEmailResponse {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut s = serializer.serialize_struct("SubmitEmailResponse", 1)?;
        s.serialize_field("id", &self.id.as_uuid().to_string())?;
        s.end()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BodyConversionError {
    #[error("invalid plain body")]
    InvalidPlain(#[source] InvalidPlainBody),
    #[error("invalid remote url")]
    InvalidRemoteUrl(#[source] anyhow::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum EnvelopeConversionError {
    #[error(transparent)]
    Body(#[from] BodyConversionError),
    #[error("invalid sender address")]
    InvalidSender(#[source] anyhow::Error),
    #[error("recipients must not be empty")]
    EmptyRecipients,
    #[error("invalid recipient address")]
    InvalidRecipient(#[source] anyhow::Error),
    #[error("invalid attachment base64")]
    InvalidAttachmentBase64 {
        filename: String,
        #[source]
        source: anyhow::Error,
    },
    #[error("invalid attachment shape: exactly one of inline_base64 or url must be set")]
    InvalidAttachmentShape { filename: String },
    #[error("invalid attachment url")]
    InvalidAttachmentUrl {
        filename: String,
        #[source]
        source: anyhow::Error,
    },
    #[error("too many attachments")]
    TooManyAttachments,
    #[error("attachment too large")]
    AttachmentTooLarge { filename: String },
}

impl SubmitEmailRequest {
    pub fn into_submit_input(self) -> Result<SubmitEmailInput, EnvelopeConversionError> {
        use std::str::FromStr;
        email_address::EmailAddress::from_str(&self.sender)
            .context("parsing sender")
            .map_err(EnvelopeConversionError::InvalidSender)?;
        if self.recipients.is_empty() {
            return Err(EnvelopeConversionError::EmptyRecipients);
        }
        let recipients = self
            .recipients
            .into_iter()
            .map(|r| {
                email_address::EmailAddress::from_str(&r.address)
                    .context("parsing recipient")
                    .map_err(EnvelopeConversionError::InvalidRecipient)?;
                Ok((r.kind.into(), r.address))
            })
            .collect::<Result<Vec<_>, EnvelopeConversionError>>()?;
        let body = self.body.try_into()?;

        if self.attachments.len() > MAX_ATTACHMENTS_PER_EMAIL {
            return Err(EnvelopeConversionError::TooManyAttachments);
        }
        let mut atts = Vec::with_capacity(self.attachments.len());
        for a in self.attachments {
            let att = match (a.inline_base64, a.url) {
                (Some(b64), None) => {
                    let decoded = base64::engine::general_purpose::STANDARD
                        .decode(&b64)
                        .context(format!("decoding base64 for attachment '{}'", a.filename))
                        .map_err(|source| EnvelopeConversionError::InvalidAttachmentBase64 {
                            filename: a.filename.clone(),
                            source,
                        })?;
                    if decoded.len() as u64 > MAX_ATTACHMENT_BYTES {
                        return Err(EnvelopeConversionError::AttachmentTooLarge {
                            filename: a.filename,
                        });
                    }
                    AttachmentInput::Inline {
                        filename: a.filename,
                        content_type: a.content_type,
                        bytes: bytes::Bytes::from(decoded),
                    }
                }
                (None, Some(raw_url)) => {
                    let parsed = url::Url::parse(&raw_url)
                        .context(format!("parsing url for attachment '{}'", a.filename))
                        .map_err(|source| EnvelopeConversionError::InvalidAttachmentUrl {
                            filename: a.filename.clone(),
                            source,
                        })?;
                    AttachmentInput::Remote {
                        filename: a.filename,
                        content_type: a.content_type,
                        url: parsed,
                    }
                }
                _ => {
                    return Err(EnvelopeConversionError::InvalidAttachmentShape {
                        filename: a.filename,
                    });
                }
            };
            atts.push(att);
        }

        Ok(SubmitEmailInput {
            idempotency_key: self.idempotency_key,
            subject: self.subject,
            sender: self.sender,
            recipients,
            body,
            variables: self.variables,
            attachments: atts,
        })
    }
}

impl From<RecipientKindDto> for RecipientKind {
    fn from(k: RecipientKindDto) -> Self {
        match k {
            RecipientKindDto::To => Self::To,
            RecipientKindDto::Cc => Self::Cc,
            RecipientKindDto::Bcc => Self::Bcc,
        }
    }
}

fn plain_body(
    text: Option<String>,
    html: Option<String>,
) -> Result<BodySource, BodyConversionError> {
    Plain::try_new(text, html)
        .map(BodySource::Plain)
        .map_err(BodyConversionError::InvalidPlain)
}

fn remote_mjml_body(url: &str) -> Result<BodySource, BodyConversionError> {
    url::Url::parse(url)
        .context("parsing mjml remote url")
        .map(|parsed| BodySource::Mjml(MjmlSource::Remote(parsed)))
        .map_err(BodyConversionError::InvalidRemoteUrl)
}

impl TryFrom<BodyDto> for BodySource {
    type Error = BodyConversionError;

    fn try_from(b: BodyDto) -> Result<Self, Self::Error> {
        match b {
            BodyDto::Plain { text, html } => plain_body(text, html),
            BodyDto::MjmlInline { source } => Ok(Self::Mjml(MjmlSource::Inline(source))),
            BodyDto::MjmlNamed { name } => Ok(Self::Mjml(MjmlSource::Named(name))),
            BodyDto::MjmlRemote { url } => remote_mjml_body(&url),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AttachmentDto, BodyConversionError, BodyDto, BodySource, EnvelopeConversionError,
        MjmlSource, RecipientDto, RecipientKindDto, SubmitEmailRequest,
    };

    fn base_request() -> SubmitEmailRequest {
        SubmitEmailRequest {
            idempotency_key: None,
            subject: None,
            sender: "a@b.c".into(),
            recipients: vec![RecipientDto {
                kind: RecipientKindDto::To,
                address: "t@x.y".into(),
            }],
            body: BodyDto::Plain {
                text: Some("hi".into()),
                html: None,
            },
            variables: serde_json::Map::new(),
            attachments: vec![],
        }
    }

    #[test]
    fn plain_with_text_converts_to_plain_body() {
        let dto = BodyDto::Plain {
            text: Some("hello".into()),
            html: None,
        };
        assert!(matches!(
            BodySource::try_from(dto),
            Ok(BodySource::Plain(_))
        ));
    }

    #[test]
    fn plain_with_neither_text_nor_html_returns_invalid_plain() {
        let dto = BodyDto::Plain {
            text: None,
            html: None,
        };
        assert!(matches!(
            BodySource::try_from(dto),
            Err(BodyConversionError::InvalidPlain(_))
        ));
    }

    #[test]
    fn mjml_inline_converts_to_mjml_inline_source() {
        let dto = BodyDto::MjmlInline {
            source: "<mjml/>".into(),
        };
        assert!(matches!(
            BodySource::try_from(dto),
            Ok(BodySource::Mjml(MjmlSource::Inline(_)))
        ));
    }

    #[test]
    fn mjml_named_converts_to_mjml_named_source() {
        let dto = BodyDto::MjmlNamed {
            name: "welcome".into(),
        };
        assert!(matches!(
            BodySource::try_from(dto),
            Ok(BodySource::Mjml(MjmlSource::Named(_)))
        ));
    }

    #[test]
    fn mjml_remote_with_valid_url_converts_to_mjml_remote_source() {
        let dto = BodyDto::MjmlRemote {
            url: "https://example.com/template.mjml".into(),
        };
        assert!(matches!(
            BodySource::try_from(dto),
            Ok(BodySource::Mjml(MjmlSource::Remote(_)))
        ));
    }

    #[test]
    fn mjml_remote_with_invalid_url_returns_invalid_remote_url() {
        let dto = BodyDto::MjmlRemote {
            url: "not a url".into(),
        };
        assert!(matches!(
            BodySource::try_from(dto),
            Err(BodyConversionError::InvalidRemoteUrl(_))
        ));
    }

    #[test]
    fn invalid_sender_returns_error() {
        let req = SubmitEmailRequest {
            sender: "not-an-email".into(),
            ..base_request()
        };
        assert!(matches!(
            req.into_submit_input(),
            Err(EnvelopeConversionError::InvalidSender(_))
        ));
    }

    #[test]
    fn empty_recipients_returns_error() {
        let req = SubmitEmailRequest {
            recipients: vec![],
            ..base_request()
        };
        assert!(matches!(
            req.into_submit_input(),
            Err(EnvelopeConversionError::EmptyRecipients)
        ));
    }

    #[test]
    fn invalid_recipient_returns_error() {
        let req = SubmitEmailRequest {
            recipients: vec![RecipientDto {
                kind: RecipientKindDto::To,
                address: "bad".into(),
            }],
            ..base_request()
        };
        assert!(matches!(
            req.into_submit_input(),
            Err(EnvelopeConversionError::InvalidRecipient(_))
        ));
    }

    #[test]
    fn valid_base64_attachment_decodes_correctly() {
        use base64::Engine;
        use catapulte_domain::use_case::submit_email::AttachmentInput;
        let content = b"hello attachment";
        let encoded = base64::engine::general_purpose::STANDARD.encode(content);
        let req = SubmitEmailRequest {
            attachments: vec![AttachmentDto {
                filename: "test.txt".into(),
                content_type: "text/plain".into(),
                inline_base64: Some(encoded),
                url: None,
            }],
            ..base_request()
        };
        let input = req.into_submit_input().unwrap();
        assert_eq!(input.attachments.len(), 1);
        let AttachmentInput::Inline { bytes, .. } = &input.attachments[0] else {
            panic!("expected Inline variant");
        };
        assert_eq!(bytes.as_ref(), content);
    }

    #[test]
    fn invalid_base64_attachment_returns_error() {
        let req = SubmitEmailRequest {
            attachments: vec![AttachmentDto {
                filename: "bad.txt".into(),
                content_type: "text/plain".into(),
                inline_base64: Some("not valid base64!!!".into()),
                url: None,
            }],
            ..base_request()
        };
        assert!(matches!(
            req.into_submit_input(),
            Err(EnvelopeConversionError::InvalidAttachmentBase64 { .. })
        ));
    }

    #[test]
    fn too_many_attachments_returns_error() {
        use super::MAX_ATTACHMENTS_PER_EMAIL;
        use base64::Engine;
        let encoded = base64::engine::general_purpose::STANDARD.encode(b"x");
        let attachments = (0..=MAX_ATTACHMENTS_PER_EMAIL)
            .map(|i| AttachmentDto {
                filename: format!("file{i}.txt"),
                content_type: "text/plain".into(),
                inline_base64: Some(encoded.clone()),
                url: None,
            })
            .collect();
        let req = SubmitEmailRequest {
            attachments,
            ..base_request()
        };
        assert!(matches!(
            req.into_submit_input(),
            Err(EnvelopeConversionError::TooManyAttachments)
        ));
    }

    #[test]
    fn oversize_attachment_returns_error() {
        use super::MAX_ATTACHMENT_BYTES;
        use base64::Engine;
        let big = vec![0u8; (MAX_ATTACHMENT_BYTES + 1) as usize];
        let encoded = base64::engine::general_purpose::STANDARD.encode(&big);
        let req = SubmitEmailRequest {
            attachments: vec![AttachmentDto {
                filename: "big.bin".into(),
                content_type: "application/octet-stream".into(),
                inline_base64: Some(encoded),
                url: None,
            }],
            ..base_request()
        };
        assert!(matches!(
            req.into_submit_input(),
            Err(EnvelopeConversionError::AttachmentTooLarge { .. })
        ));
    }

    #[test]
    fn remote_url_attachment_builds_remote_variant() {
        use catapulte_domain::use_case::submit_email::AttachmentInput;
        let req = SubmitEmailRequest {
            attachments: vec![AttachmentDto {
                filename: "remote.pdf".into(),
                content_type: "application/pdf".into(),
                inline_base64: None,
                url: Some("https://example.com/file.pdf".into()),
            }],
            ..base_request()
        };
        let input = req.into_submit_input().unwrap();
        assert_eq!(input.attachments.len(), 1);
        assert!(matches!(
            input.attachments[0],
            AttachmentInput::Remote { .. }
        ));
    }

    #[test]
    fn both_inline_and_url_returns_invalid_shape() {
        use base64::Engine;
        let encoded = base64::engine::general_purpose::STANDARD.encode(b"x");
        let req = SubmitEmailRequest {
            attachments: vec![AttachmentDto {
                filename: "both.txt".into(),
                content_type: "text/plain".into(),
                inline_base64: Some(encoded),
                url: Some("https://example.com/file.txt".into()),
            }],
            ..base_request()
        };
        assert!(matches!(
            req.into_submit_input(),
            Err(EnvelopeConversionError::InvalidAttachmentShape { .. })
        ));
    }

    #[test]
    fn neither_inline_nor_url_returns_invalid_shape() {
        let req = SubmitEmailRequest {
            attachments: vec![AttachmentDto {
                filename: "neither.txt".into(),
                content_type: "text/plain".into(),
                inline_base64: None,
                url: None,
            }],
            ..base_request()
        };
        assert!(matches!(
            req.into_submit_input(),
            Err(EnvelopeConversionError::InvalidAttachmentShape { .. })
        ));
    }

    #[test]
    fn malformed_url_returns_invalid_attachment_url() {
        let req = SubmitEmailRequest {
            attachments: vec![AttachmentDto {
                filename: "bad-url.txt".into(),
                content_type: "text/plain".into(),
                inline_base64: None,
                url: Some("not a url at all".into()),
            }],
            ..base_request()
        };
        assert!(matches!(
            req.into_submit_input(),
            Err(EnvelopeConversionError::InvalidAttachmentUrl { .. })
        ));
    }
}

#[derive(Debug, Deserialize)]
pub struct ListEventsQuery {
    #[serde(default)]
    pub email_id: Option<String>,
    #[serde(default)]
    pub event_type: Option<String>,
    #[serde(default)]
    pub after_ms: Option<i64>,
    #[serde(default)]
    pub before_ms: Option<i64>,
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub offset: Option<u32>,
}

pub const DEFAULT_EVENTS_LIMIT: u32 = 20;
pub const MAX_EVENTS_LIMIT: u32 = 100;

#[derive(Debug, Serialize)]
pub struct EventRecordDto {
    pub id: String,
    pub email_id: String,
    pub event_type: String,
    pub payload: Option<serde_json::Value>,
    pub sender_name: Option<String>,
    pub created_at_ms: i64,
}

impl From<catapulte_domain::port::event_repository::EventRecord> for EventRecordDto {
    fn from(r: catapulte_domain::port::event_repository::EventRecord) -> Self {
        Self {
            id: r.id.to_string(),
            email_id: r.email_id.as_uuid().to_string(),
            event_type: r.event_type,
            payload: r.payload,
            sender_name: r.sender_name.map(|n| n.as_str().to_owned()),
            created_at_ms: r.created_at_ms,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ListEventsResponse {
    pub events: Vec<EventRecordDto>,
    pub limit: u32,
    pub offset: u32,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmailStatusDto {
    Queued,
    Sent,
    Failed,
}

impl From<EmailStatusDto> for catapulte_domain::port::email_repository::EmailStatus {
    fn from(s: EmailStatusDto) -> Self {
        match s {
            EmailStatusDto::Queued => Self::Queued,
            EmailStatusDto::Sent => Self::Sent,
            EmailStatusDto::Failed => Self::Failed,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct ListEmailsQuery {
    #[serde(default)]
    pub status: Option<EmailStatusDto>,
    #[serde(default)]
    pub after_ms: Option<i64>,
    #[serde(default)]
    pub before_ms: Option<i64>,
    #[serde(default)]
    pub recipient: Option<String>,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub offset: Option<u32>,
}

pub const DEFAULT_EMAILS_LIMIT: u32 = 20;
pub const MAX_EMAILS_LIMIT: u32 = 100;

#[derive(Debug, Serialize)]
pub struct RecipientResponseDto {
    pub kind: String,
    pub address: String,
}

#[derive(Debug, Serialize)]
pub struct EmailRecordDto {
    pub id: String,
    pub idempotency_key: Option<String>,
    pub subject: Option<String>,
    pub sender: String,
    pub recipients: Vec<RecipientResponseDto>,
    pub created_at_ms: i64,
    pub status: String,
}

impl From<catapulte_domain::port::email_repository::EmailRecord> for EmailRecordDto {
    fn from(r: catapulte_domain::port::email_repository::EmailRecord) -> Self {
        use catapulte_domain::entity::email::RecipientKind;
        use catapulte_domain::port::email_repository::EmailStatus;
        let status = match r.status {
            EmailStatus::Queued => "queued",
            EmailStatus::Sent => "sent",
            EmailStatus::Failed => "failed",
        };
        let recipients = r
            .recipients
            .into_iter()
            .map(|(kind, address)| RecipientResponseDto {
                kind: match kind {
                    RecipientKind::To => "to".to_owned(),
                    RecipientKind::Cc => "cc".to_owned(),
                    RecipientKind::Bcc => "bcc".to_owned(),
                },
                address,
            })
            .collect();
        Self {
            id: r.id.as_uuid().to_string(),
            idempotency_key: r.idempotency_key,
            subject: r.subject,
            sender: r.sender,
            recipients,
            created_at_ms: r.created_at_ms,
            status: status.to_owned(),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ListEmailsResponse {
    pub emails: Vec<EmailRecordDto>,
    pub limit: u32,
    pub offset: u32,
}

#[derive(Serialize)]
pub struct SenderQuotaDto {
    pub count: u64,
    pub range: String,
}

#[derive(Serialize)]
pub struct SenderDto {
    pub name: String,
    pub sent_in_range: u64,
    pub failed_in_range: u64,
    pub quota: Option<SenderQuotaDto>,
}

#[derive(Serialize)]
pub struct ListSendersResponse {
    pub senders: Vec<SenderDto>,
}
