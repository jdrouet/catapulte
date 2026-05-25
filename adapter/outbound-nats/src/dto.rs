use anyhow::Context;
use catapulte_domain::entity::attachment::{AttachmentRef, BlobRef};
use catapulte_domain::entity::body::{BodySource, MjmlSource, Plain};
use catapulte_domain::entity::email::{EmailId, RecipientKind};
use catapulte_domain::entity::envelope::Envelope;
use serde::{Deserialize, Serialize};

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
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct QueuedEmailPayload {
    pub id: uuid::Uuid,
    pub envelope: EnvelopeDto,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct EnvelopeDto {
    pub idempotency_key: Option<String>,
    pub subject: Option<String>,
    pub sender: String,
    pub recipients: Vec<RecipientDto>,
    pub body: BodySourceDto,
    pub variables: serde_json::Map<String, serde_json::Value>,
    #[serde(default)]
    pub attachments: Vec<AttachmentRefDto>,
}

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

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecipientKindDto {
    To,
    Cc,
    Bcc,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RecipientDto {
    pub kind: RecipientKindDto,
    pub address: String,
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

impl From<RecipientKindDto> for RecipientKind {
    fn from(dto: RecipientKindDto) -> Self {
        match dto {
            RecipientKindDto::To => RecipientKind::To,
            RecipientKindDto::Cc => RecipientKind::Cc,
            RecipientKindDto::Bcc => RecipientKind::Bcc,
        }
    }
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

impl From<(&EmailId, &Envelope)> for QueuedEmailPayload {
    fn from((id, envelope): (&EmailId, &Envelope)) -> Self {
        Self {
            id: id.as_uuid(),
            envelope: EnvelopeDto {
                idempotency_key: envelope.idempotency_key.clone(),
                subject: envelope.subject.clone(),
                sender: envelope.sender.clone(),
                recipients: envelope
                    .recipients
                    .iter()
                    .map(|(k, a)| RecipientDto {
                        kind: (*k).into(),
                        address: a.clone(),
                    })
                    .collect(),
                body: BodySourceDto::from(&envelope.body),
                variables: envelope.variables.clone(),
                attachments: envelope
                    .attachments
                    .iter()
                    .map(AttachmentRefDto::from)
                    .collect(),
            },
        }
    }
}

impl TryFrom<QueuedEmailPayload> for (EmailId, Envelope) {
    type Error = anyhow::Error;

    fn try_from(payload: QueuedEmailPayload) -> Result<Self, Self::Error> {
        let body = BodySource::try_from(payload.envelope.body)?;
        let recipients = payload
            .envelope
            .recipients
            .into_iter()
            .map(|r| (RecipientKind::from(r.kind), r.address))
            .collect();
        let attachments = payload
            .envelope
            .attachments
            .into_iter()
            .map(AttachmentRef::from)
            .collect();
        let envelope = Envelope {
            idempotency_key: payload.envelope.idempotency_key,
            subject: payload.envelope.subject,
            sender: payload.envelope.sender,
            recipients,
            body,
            variables: payload.envelope.variables,
            attachments,
        };
        Ok((EmailId::from(payload.id), envelope))
    }
}

#[cfg(test)]
mod tests {
    use catapulte_domain::entity::attachment::{AttachmentRef, BlobRef};
    use catapulte_domain::entity::body::{BodySource, Plain};
    use catapulte_domain::entity::email::{EmailId, RecipientKind};
    use catapulte_domain::entity::envelope::Envelope;

    use super::QueuedEmailPayload;

    #[test]
    fn envelope_serialization_stays_under_256_kb_with_many_attachments() {
        let attachments: Vec<AttachmentRef> = (0..100)
            .map(|i| AttachmentRef {
                filename: format!("file-{i}.pdf"),
                content_type: "application/pdf".into(),
                size_bytes: 25 * 1024 * 1024,
                blob: BlobRef {
                    backend: "fs".into(),
                    key: uuid::Uuid::now_v7().simple().to_string(),
                },
            })
            .collect();

        let id = EmailId::default();
        let envelope = Envelope {
            idempotency_key: None,
            subject: Some("Test subject".into()),
            sender: "sender@example.com".into(),
            recipients: vec![(RecipientKind::To, "to@example.com".into())],
            body: BodySource::Plain(Plain::try_new(Some("Hello world".into()), None).unwrap()),
            variables: serde_json::Map::new(),
            attachments,
        };

        let payload = QueuedEmailPayload::from((&id, &envelope));
        let bytes = serde_json::to_vec(&payload).unwrap();
        assert!(
            bytes.len() < 256 * 1024,
            "envelope serialization exceeded 256 KB: {} bytes",
            bytes.len()
        );
    }
}
