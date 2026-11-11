use anyhow::Context;
use catapulte_domain::entity::email::EmailId;
use catapulte_domain::entity::lifecycle_event::EventContext;
use catapulte_domain::port::email_lookup::{EmailLookup, EmailLookupError, EmailSummary};
use sqlx::Row;

use crate::SqliteAdapter;
use crate::dto::{EnvelopeExtrasDto, RecipientDto, recipients_from_dto};

impl EmailLookup for SqliteAdapter {
    async fn find(&self, id: EmailId) -> Result<Option<EmailSummary>, EmailLookupError> {
        let storage = |source| EmailLookupError::Storage { source };
        let row =
            sqlx::query("SELECT correlation_id, recipients, tags, extras FROM emails WHERE id = ?")
                .bind(id.as_uuid().as_bytes().to_vec())
                .fetch_optional(self.pool())
                .await
                .context("looking up email")
                .map_err(storage)?;
        let Some(row) = row else {
            return Ok(None);
        };
        let summary = (|| -> anyhow::Result<EmailSummary> {
            let recipients: sqlx::types::Json<Vec<RecipientDto>> =
                row.try_get("recipients").context("reading recipients")?;
            let tags: Option<sqlx::types::Json<Vec<String>>> =
                row.try_get("tags").context("reading tags")?;
            let extras: Option<sqlx::types::Json<EnvelopeExtrasDto>> =
                row.try_get("extras").context("reading extras")?;
            Ok(EmailSummary {
                recipients: recipients_from_dto(recipients.0),
                context: EventContext {
                    correlation_id: row
                        .try_get("correlation_id")
                        .context("reading correlation_id")?,
                    tags: tags.map(|j| j.0).unwrap_or_default(),
                    metadata: extras.map(|j| j.0.metadata).unwrap_or_default(),
                },
            })
        })()
        .map_err(storage)?;
        Ok(Some(summary))
    }
}

#[cfg(test)]
mod tests {
    use catapulte_domain::entity::body::{BodySource, Plain};
    use catapulte_domain::entity::email::{EmailId, RecipientKind};
    use catapulte_domain::entity::envelope::Envelope;
    use catapulte_domain::port::email_lookup::EmailLookup;
    use catapulte_domain::port::email_repository::EmailRepository;

    use crate::SqliteAdapter;

    #[tokio::test]
    async fn find_returns_recipients_and_context() {
        let adapter = SqliteAdapter::connect(":memory:").await.unwrap();
        adapter.migrate().await.unwrap();
        let id = EmailId::default();
        let mut envelope = Envelope::new(
            "s@example.com",
            vec![
                (RecipientKind::To, "a@x.com".to_owned()),
                (RecipientKind::Bcc, "b@x.com".to_owned()),
            ],
            BodySource::Plain(Plain::try_new(Some("hi".to_owned()), None).unwrap()),
        );
        envelope.correlation_id = Some("corr".to_owned());
        envelope.tags = vec!["welcome".to_owned()];
        envelope
            .metadata
            .insert("order".to_owned(), "42".to_owned());
        adapter.save(id, &envelope).await.unwrap();

        let found = adapter.find(id).await.unwrap().unwrap();

        assert_eq!(found.recipients.len(), 2);
        assert_eq!(found.context.correlation_id.as_deref(), Some("corr"));
        assert_eq!(found.context.tags, vec!["welcome".to_owned()]);
        assert_eq!(found.context.metadata["order"], "42");
        assert!(adapter.find(EmailId::default()).await.unwrap().is_none());
    }
}
