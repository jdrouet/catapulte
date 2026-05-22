use crate::entity::email::EmailId;
use crate::entity::envelope::Envelope;

pub struct SubmitParams {}

#[derive(Debug, Default)]
pub struct SubmitEmailService;

impl SubmitEmailService {
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    #[allow(clippy::unused_async)]
    pub async fn execute(&self, _envelope: Envelope, _params: SubmitParams) -> EmailId {
        // TODO: validate, persist, emit queued event
        EmailId::default()
    }
}

#[cfg(test)]
mod tests {
    use crate::entity::body::Plain;
    use crate::entity::email::{EmailId, RecipientKind};

    use crate::entity::envelope::Envelope;

    use super::{SubmitEmailService, SubmitParams};

    #[tokio::test]
    async fn execute_returns_an_email_id() {
        let service = SubmitEmailService::new();
        let envelope = Envelope {
            idempotency_key: None,
            sender: "sender@example.com".into(),
            recipients: vec![(RecipientKind::To, "to@example.com".into())],
            body: crate::entity::body::BodySource::Plain(
                Plain::try_new(Some("hello".into()), None).unwrap(),
            ),
            variables: serde_json::Map::new(),
        };
        let id1 = service.execute(envelope, SubmitParams {}).await;
        let id2 = EmailId::default();
        assert_ne!(id1, id2);
    }
}
