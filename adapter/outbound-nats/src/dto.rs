use anyhow::Context;
use catapulte_domain::entity::body::{BodySource, MjmlSource, Plain};
use catapulte_domain::entity::email::{EmailId, RecipientKind};
use catapulte_domain::entity::envelope::Envelope;
use serde::{Deserialize, Serialize};

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
        let envelope = Envelope {
            idempotency_key: payload.envelope.idempotency_key,
            subject: payload.envelope.subject,
            sender: payload.envelope.sender,
            recipients,
            body,
            variables: payload.envelope.variables,
        };
        Ok((EmailId::from(payload.id), envelope))
    }
}
