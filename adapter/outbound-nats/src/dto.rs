use anyhow::Context;
use catapulte_domain::entity::attachment::{AttachmentRef, BlobRef};
use catapulte_domain::entity::body::{BodySource, MjmlSource, Plain};
use catapulte_domain::entity::email::{EmailId, RecipientKind};
use catapulte_domain::entity::envelope::{Envelope, Unsubscribe};
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

#[derive(Debug, Serialize, Deserialize)]
pub struct QueuedEmailPayload {
    pub id: uuid::Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    pub envelope: EnvelopeDto,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct EnvelopeDto {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub owner: String,
    pub idempotency_key: Option<String>,
    pub subject: Option<String>,
    pub sender: String,
    pub recipients: Vec<RecipientDto>,
    pub body: BodySourceDto,
    pub variables: serde_json::Map<String, serde_json::Value>,
    #[serde(default)]
    pub attachments: Vec<AttachmentRefDto>,
    #[serde(default)]
    pub reply_to: Vec<String>,
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unsubscribe: Option<UnsubscribeDto>,
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
            correlation_id: envelope.correlation_id.clone(),
            envelope: EnvelopeDto {
                owner: envelope.owner.clone(),
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
                reply_to: envelope.reply_to.clone(),
                headers: envelope.headers.clone(),
                unsubscribe: envelope.unsubscribe.as_ref().map(UnsubscribeDto::from),
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
            owner: payload.envelope.owner,
            idempotency_key: payload.envelope.idempotency_key,
            correlation_id: payload.correlation_id,
            subject: payload.envelope.subject,
            sender: payload.envelope.sender,
            recipients,
            body,
            variables: payload.envelope.variables,
            attachments,
            reply_to: payload.envelope.reply_to,
            headers: payload.envelope.headers,
            unsubscribe: payload
                .envelope
                .unsubscribe
                .map(Unsubscribe::try_from)
                .transpose()?,
        };
        Ok((EmailId::from(payload.id), envelope))
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UnsubscribeDto {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mailto: Option<String>,
}

impl From<&Unsubscribe> for UnsubscribeDto {
    fn from(u: &Unsubscribe) -> Self {
        Self {
            url: u.url.as_ref().map(ToString::to_string),
            mailto: u.mailto.clone(),
        }
    }
}

impl TryFrom<UnsubscribeDto> for Unsubscribe {
    type Error = anyhow::Error;

    fn try_from(dto: UnsubscribeDto) -> Result<Self, Self::Error> {
        Ok(Self {
            url: dto
                .url
                .map(|u| u.parse())
                .transpose()
                .context("parsing unsubscribe url")?,
            mailto: dto.mailto,
        })
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
                content_id: None,
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
            subject: Some("Test subject".into()),
            attachments,
            ..Envelope::new(
                "sender@example.com",
                vec![(RecipientKind::To, "to@example.com".into())],
                BodySource::Plain(Plain::try_new(Some("Hello world".into()), None).unwrap()),
            )
        };

        let payload = QueuedEmailPayload::from((&id, &envelope));
        let bytes = serde_json::to_vec(&payload).unwrap();
        assert!(
            bytes.len() < 256 * 1024,
            "envelope serialization exceeded 256 KB: {} bytes",
            bytes.len()
        );
    }

    #[test]
    fn reply_to_and_headers_round_trip() {
        let id = EmailId::default();
        let envelope = Envelope {
            reply_to: vec!["Support <help@example.com>".into()],
            headers: vec![("X-Order-Id".into(), "42".into())],
            ..Envelope::new(
                "sender@example.com",
                vec![(RecipientKind::To, "to@example.com".into())],
                BodySource::Plain(Plain::try_new(Some("hi".into()), None).unwrap()),
            )
        };

        let bytes = serde_json::to_vec(&QueuedEmailPayload::from((&id, &envelope))).unwrap();
        let (_, back) = <(EmailId, Envelope)>::try_from(
            serde_json::from_slice::<QueuedEmailPayload>(&bytes).unwrap(),
        )
        .unwrap();

        assert_eq!(back.reply_to, envelope.reply_to);
        assert_eq!(back.headers, envelope.headers);
    }

    #[test]
    fn payload_without_extras_defaults_to_empty() {
        let json = serde_json::json!({
            "id": uuid::Uuid::nil(),
            "envelope": {
                "idempotency_key": null, "subject": null, "sender": "a@b.c",
                "recipients": [], "body": {"kind": "plain", "text": "x", "html": null},
                "variables": {}
            }
        });
        let payload: QueuedEmailPayload = serde_json::from_value(json).unwrap();
        let (_, env) = <(EmailId, Envelope)>::try_from(payload).unwrap();
        assert!(env.reply_to.is_empty() && env.headers.is_empty());
    }

    #[test]
    fn attachment_ref_dto_content_id_roundtrips_and_legacy_json_loads() {
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

    #[test]
    fn unsubscribe_round_trips_and_legacy_payload_loads() {
        use catapulte_domain::entity::envelope::Unsubscribe;

        let id = EmailId::default();
        let unsubscribe = Unsubscribe {
            url: Some("https://example.com/u/1".parse().unwrap()),
            mailto: Some("u@example.com".to_owned()),
        };
        let envelope = Envelope {
            unsubscribe: Some(unsubscribe.clone()),
            ..Envelope::new(
                "sender@example.com",
                vec![(RecipientKind::To, "to@example.com".into())],
                BodySource::Plain(Plain::try_new(Some("hi".into()), None).unwrap()),
            )
        };
        let bytes = serde_json::to_vec(&QueuedEmailPayload::from((&id, &envelope))).unwrap();
        let (_, back) = <(EmailId, Envelope)>::try_from(
            serde_json::from_slice::<QueuedEmailPayload>(&bytes).unwrap(),
        )
        .unwrap();
        assert_eq!(back.unsubscribe, Some(unsubscribe));

        let legacy = serde_json::json!({
            "id": uuid::Uuid::nil(),
            "envelope": {
                "idempotency_key": null, "subject": null, "sender": "a@b.c",
                "recipients": [], "body": {"kind": "plain", "text": "x", "html": null},
                "variables": {}
            }
        });
        let payload: QueuedEmailPayload = serde_json::from_value(legacy).unwrap();
        let (_, env) = <(EmailId, Envelope)>::try_from(payload).unwrap();
        assert!(env.unsubscribe.is_none());
    }
}
