use anyhow::Context;
use catapulte_domain::entity::email::EmailId;
use catapulte_domain::entity::lifecycle_event::EventContext;
use catapulte_domain::port::email_lookup::{EmailLookup, EmailLookupError, EmailSummary};
use sqlx::Row;

use crate::PostgresAdapter;
use crate::dto::{EnvelopeExtrasDto, RecipientDto, recipients_from_dto};

impl EmailLookup for PostgresAdapter {
    async fn find(&self, id: EmailId) -> Result<Option<EmailSummary>, EmailLookupError> {
        let storage = |source| EmailLookupError::Storage { source };
        let row = sqlx::query(
            "SELECT api_key, correlation_id, recipients, tags, extras FROM emails WHERE id = $1",
        )
        .bind(id.as_uuid())
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
                owner: row.try_get("api_key").context("reading api_key")?,
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

    use crate::PostgresAdapter;

    async fn wait_for_tcp(port: u16, timeout: std::time::Duration) {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .is_ok()
            {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "127.0.0.1:{port} did not accept connections within {timeout:?}"
            );
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }

    async fn fresh_adapter() -> PostgresAdapter {
        use testcontainers::GenericImage;
        use testcontainers::ImageExt;
        use testcontainers::core::WaitFor;
        use testcontainers::runners::AsyncRunner;

        let pg = GenericImage::new("postgres", "16-alpine")
            .with_wait_for(WaitFor::message_on_stderr(
                "database system is ready to accept connections",
            ))
            .with_env_var("POSTGRES_USER", "catapulte")
            .with_env_var("POSTGRES_PASSWORD", "catapulte")
            .with_env_var("POSTGRES_DB", "catapulte")
            .start()
            .await
            .expect("failed to start postgres container; ensure Docker is running");

        let port = pg.get_host_port_ipv4(5432u16).await.unwrap();
        wait_for_tcp(port, std::time::Duration::from_secs(15)).await;
        let url = format!("postgres://catapulte:catapulte@127.0.0.1:{port}/catapulte");
        let adapter = PostgresAdapter::connect(&url, 10, std::time::Duration::from_secs(30))
            .await
            .unwrap();
        adapter.migrate().await.unwrap();
        std::mem::forget(pg);
        adapter
    }

    #[tokio::test]
    async fn find_returns_recipients_owner_and_context() {
        let adapter = fresh_adapter().await;
        let id = EmailId::default();
        let mut envelope = Envelope::new(
            "s@example.com",
            vec![
                (RecipientKind::To, "a@x.com".to_owned()),
                (RecipientKind::Bcc, "b@x.com".to_owned()),
            ],
            BodySource::Plain(Plain::try_new(Some("hi".to_owned()), None).unwrap()),
        );
        envelope.owner = "ci".to_owned();
        envelope.correlation_id = Some("corr".to_owned());
        envelope.tags = vec!["welcome".to_owned()];
        envelope
            .metadata
            .insert("order".to_owned(), "42".to_owned());
        adapter.save(id, &envelope).await.unwrap();

        let found = adapter.find(id).await.unwrap().unwrap();

        assert_eq!(found.owner, "ci");
        assert_eq!(found.recipients.len(), 2);
        assert_eq!(found.context.correlation_id.as_deref(), Some("corr"));
        assert_eq!(found.context.tags, vec!["welcome".to_owned()]);
        assert_eq!(found.context.metadata["order"], "42");
        assert!(adapter.find(EmailId::default()).await.unwrap().is_none());
    }
}
