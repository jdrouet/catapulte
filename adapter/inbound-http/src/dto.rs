use anyhow::Context;
use catapulte_domain::entity::body::{BodySource, InvalidPlainBody, MjmlSource, Plain};
use catapulte_domain::entity::email::{EmailId, RecipientKind};
use catapulte_domain::entity::envelope::Envelope;
use serde::ser::SerializeStruct;
use serde::{Deserialize, Serialize, Serializer};

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

impl SubmitEmailRequest {
    /// # Errors
    ///
    /// Returns `BodyConversionError::InvalidPlain` when the plain body has neither text nor html.
    /// Returns `BodyConversionError::InvalidRemoteUrl` when the mjml remote URL cannot be parsed.
    pub fn into_envelope(self) -> Result<Envelope, BodyConversionError> {
        let recipients = self
            .recipients
            .into_iter()
            .map(|r| (r.kind.into(), r.address))
            .collect();
        let body = self.body.try_into()?;
        Ok(Envelope {
            idempotency_key: self.idempotency_key,
            subject: self.subject,
            sender: self.sender,
            recipients,
            body,
            variables: self.variables,
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
    use super::{BodyConversionError, BodyDto, BodySource, MjmlSource};

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
}

#[derive(Debug, Deserialize)]
pub struct ListEventsQuery {
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
    pub created_at_ms: i64,
}

impl From<catapulte_domain::port::event_repository::EventRecord> for EventRecordDto {
    fn from(r: catapulte_domain::port::event_repository::EventRecord) -> Self {
        Self {
            id: r.id.to_string(),
            email_id: r.email_id.as_uuid().to_string(),
            event_type: r.event_type,
            payload: r.payload,
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
