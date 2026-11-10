use anyhow::Context;
use catapulte_domain::entity::attachment::{AttachmentRef, BlobRef};
use catapulte_domain::entity::body::{BodySource, MjmlSource, Plain};
use catapulte_domain::entity::email::{RecipientKind, bare_address};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BodySourceDto {
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

impl From<&BodySource> for BodySourceDto {
    fn from(body: &BodySource) -> Self {
        match body {
            BodySource::Plain(p) => Self::Plain {
                text: p.text().map(str::to_owned),
                html: p.html().map(str::to_owned),
            },
            BodySource::Mjml(MjmlSource::Inline(s)) => Self::MjmlInline { source: s.clone() },
            BodySource::Mjml(MjmlSource::Named(n)) => Self::MjmlNamed { name: n.clone() },
            BodySource::Mjml(MjmlSource::Remote(u)) => Self::MjmlRemote { url: u.to_string() },
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecipientKindDto {
    To,
    Cc,
    Bcc,
}

impl From<RecipientKind> for RecipientKindDto {
    fn from(kind: RecipientKind) -> Self {
        match kind {
            RecipientKind::To => Self::To,
            RecipientKind::Cc => Self::Cc,
            RecipientKind::Bcc => Self::Bcc,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RecipientDto {
    pub kind: RecipientKindDto,
    pub address: String,
    /// Lowercase bare address used by the recipient filter; absent on legacy rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bare: Option<String>,
}

#[must_use]
pub fn recipients_to_dto(recipients: &[(RecipientKind, String)]) -> Vec<RecipientDto> {
    recipients
        .iter()
        .map(|(k, a)| RecipientDto {
            kind: (*k).into(),
            address: a.clone(),
            bare: Some(bare_address(a).to_ascii_lowercase()),
        })
        .collect()
}

impl TryFrom<BodySourceDto> for BodySource {
    type Error = anyhow::Error;

    fn try_from(dto: BodySourceDto) -> Result<Self, Self::Error> {
        match dto {
            BodySourceDto::Plain { text, html } => Plain::try_new(text, html)
                .context("invalid plain body")
                .map(BodySource::Plain),
            BodySourceDto::MjmlInline { source } => {
                Ok(BodySource::Mjml(MjmlSource::Inline(source)))
            }
            BodySourceDto::MjmlNamed { name } => Ok(BodySource::Mjml(MjmlSource::Named(name))),
            BodySourceDto::MjmlRemote { url } => url
                .parse()
                .context("invalid remote url")
                .map(|u| BodySource::Mjml(MjmlSource::Remote(u))),
        }
    }
}

impl From<RecipientKindDto> for RecipientKind {
    fn from(dto: RecipientKindDto) -> Self {
        match dto {
            RecipientKindDto::To => RecipientKind::To,
            RecipientKindDto::Cc => RecipientKind::Cc,
            RecipientKindDto::Bcc => RecipientKind::Bcc,
        }
    }
}

#[must_use]
pub fn recipients_from_dto(dtos: Vec<RecipientDto>) -> Vec<(RecipientKind, String)> {
    dtos.into_iter()
        .map(|d| (d.kind.into(), d.address))
        .collect()
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BlobRefDto {
    pub backend: String,
    pub key: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AttachmentRefDto {
    pub filename: String,
    pub content_type: String,
    pub size_bytes: u64,
    pub blob: BlobRefDto,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_id: Option<String>,
}

impl From<&AttachmentRef> for AttachmentRefDto {
    fn from(a: &AttachmentRef) -> Self {
        Self {
            filename: a.filename.clone(),
            content_type: a.content_type.clone(),
            size_bytes: a.size_bytes,
            blob: BlobRefDto {
                backend: a.blob.backend.clone(),
                key: a.blob.key.clone(),
            },
            content_id: a.content_id.clone(),
        }
    }
}

impl From<AttachmentRefDto> for AttachmentRef {
    fn from(dto: AttachmentRefDto) -> Self {
        Self {
            filename: dto.filename,
            content_type: dto.content_type,
            size_bytes: dto.size_bytes,
            blob: BlobRef {
                backend: dto.blob.backend,
                key: dto.blob.key,
            },
            content_id: dto.content_id,
        }
    }
}

/// Wrapper stored in the `body` column. Deserialization accepts both the legacy
/// shape (a bare `BodySourceDto`) and the new shape (`{ "source": ..., "attachments": [...] }`).
#[derive(Debug, Serialize)]
pub struct EnvelopeBodyDto {
    pub source: BodySourceDto,
    #[serde(default)]
    pub attachments: Vec<AttachmentRefDto>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum EnvelopeBodyDtoDeser {
    WithAttachments {
        source: BodySourceDto,
        #[serde(default)]
        attachments: Vec<AttachmentRefDto>,
    },
    Legacy(BodySourceDto),
}

impl EnvelopeBodyDtoDeser {
    #[must_use]
    pub fn split(self) -> (BodySourceDto, Vec<AttachmentRefDto>) {
        match self {
            Self::WithAttachments {
                source,
                attachments,
            } => (source, attachments),
            Self::Legacy(source) => (source, vec![]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::EnvelopeBodyDtoDeser;

    #[test]
    fn legacy_body_json_deserializes_with_empty_attachments() {
        let json = r#"{"kind":"plain","text":"hello","html":null}"#;
        let deser: EnvelopeBodyDtoDeser =
            serde_json::from_str(json).expect("deserialization failed");
        let (_, attachments) = deser.split();
        assert!(
            attachments.is_empty(),
            "expected empty attachments for legacy shape"
        );
    }

    #[test]
    fn new_body_json_with_empty_attachments_deserializes() {
        let json = r#"{"source":{"kind":"plain","text":"hello","html":null},"attachments":[]}"#;
        let deser: EnvelopeBodyDtoDeser =
            serde_json::from_str(json).expect("deserialization failed");
        let (_, attachments) = deser.split();
        assert!(attachments.is_empty());
    }

    #[test]
    fn envelope_body_dto_roundtrip_with_attachments() {
        use catapulte_domain::entity::attachment::{AttachmentRef, BlobRef};
        use catapulte_domain::entity::body::BodySource;

        use super::{AttachmentRefDto, BlobRefDto, BodySourceDto, EnvelopeBodyDto};

        let dto = EnvelopeBodyDto {
            source: BodySourceDto::Plain {
                text: Some("hello".to_owned()),
                html: None,
            },
            attachments: vec![
                AttachmentRefDto {
                    content_id: None,
                    filename: "invoice.pdf".to_owned(),
                    content_type: "application/pdf".to_owned(),
                    size_bytes: 1024,
                    blob: BlobRefDto {
                        backend: "s3".to_owned(),
                        key: "uploads/invoice.pdf".to_owned(),
                    },
                },
                AttachmentRefDto {
                    content_id: None,
                    filename: "photo.png".to_owned(),
                    content_type: "image/png".to_owned(),
                    size_bytes: 2048,
                    blob: BlobRefDto {
                        backend: "gcs".to_owned(),
                        key: "media/photo.png".to_owned(),
                    },
                },
            ],
        };

        let value = serde_json::to_value(&dto).expect("serialization failed");
        let deser: EnvelopeBodyDtoDeser =
            serde_json::from_value(value).expect("deserialization failed");
        let (body_dto, attachment_dtos) = deser.split();

        let body = BodySource::try_from(body_dto).expect("body conversion failed");
        let attachments: Vec<AttachmentRef> = attachment_dtos
            .into_iter()
            .map(AttachmentRef::from)
            .collect();

        assert!(matches!(body, BodySource::Plain(_)));
        assert_eq!(attachments.len(), 2);

        assert_eq!(attachments[0].filename, "invoice.pdf");
        assert_eq!(attachments[0].content_type, "application/pdf");
        assert_eq!(attachments[0].size_bytes, 1024);
        assert_eq!(
            attachments[0].blob,
            BlobRef {
                backend: "s3".to_owned(),
                key: "uploads/invoice.pdf".to_owned(),
            }
        );

        assert_eq!(attachments[1].filename, "photo.png");
        assert_eq!(attachments[1].content_type, "image/png");
        assert_eq!(attachments[1].size_bytes, 2048);
        assert_eq!(
            attachments[1].blob,
            BlobRef {
                backend: "gcs".to_owned(),
                key: "media/photo.png".to_owned(),
            }
        );
    }

    #[test]
    fn attachment_ref_dto_content_id_roundtrips_and_legacy_json_loads() {
        use catapulte_domain::entity::attachment::AttachmentRef;

        use super::AttachmentRefDto;

        let legacy = r#"{"filename":"a.png","content_type":"image/png","size_bytes":3,"blob":{"backend":"fs","key":"k"}}"#;
        let dto: AttachmentRefDto = serde_json::from_str(legacy).unwrap();
        assert_eq!(AttachmentRef::from(dto).content_id, None);

        let mut dto: AttachmentRefDto = serde_json::from_str(legacy).unwrap();
        assert!(!serde_json::to_string(&dto).unwrap().contains("content_id"));

        dto.content_id = Some("logo".to_owned());
        let json = serde_json::to_string(&dto).unwrap();
        let back: AttachmentRefDto = serde_json::from_str(&json).unwrap();
        assert_eq!(
            AttachmentRef::from(back).content_id.as_deref(),
            Some("logo")
        );
    }
}

/// Optional envelope fields stored in the `extras` column; a NULL column reads as the default.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct EnvelopeExtrasDto {
    #[serde(default)]
    pub reply_to: Vec<String>,
    #[serde(default)]
    pub headers: Vec<(String, String)>,
}
