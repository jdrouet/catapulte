use anyhow::Context;
use catapulte_domain::entity::email::EmailId;
use catapulte_domain::entity::envelope::Envelope;
use catapulte_domain::port::email_repository::{EmailRepository, EmailRepositoryError};
use sqlx::types::Json;

use crate::SqliteAdapter;
use crate::dto::{BodySourceDto, recipients_to_dto};

impl EmailRepository for SqliteAdapter {
    /// # Errors
    ///
    /// Returns `EmailRepositoryError::Storage` when the database insert fails.
    async fn save(&self, id: EmailId, envelope: &Envelope) -> Result<(), EmailRepositoryError> {
        let id_bytes = id.as_uuid().as_bytes().to_vec();
        let body_dto = BodySourceDto::from(&envelope.body);
        let recipients_dto = recipients_to_dto(&envelope.recipients);

        sqlx::query(
            "INSERT INTO emails (id, idempotency_key, subject, sender, recipients, body, variables) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(id_bytes)
        .bind(envelope.idempotency_key.as_deref())
        .bind(envelope.subject.as_deref())
        .bind(&envelope.sender)
        .bind(Json(&recipients_dto))
        .bind(Json(&body_dto))
        .bind(Json(&envelope.variables))
        .execute(self.pool())
        .await
        .context("inserting email")
        .map_err(|source| EmailRepositoryError::Storage { source })?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use catapulte_domain::entity::body::{BodySource, MjmlSource, Plain};
    use catapulte_domain::entity::email::EmailId;
    use catapulte_domain::entity::envelope::Envelope;
    use catapulte_domain::port::email_repository::EmailRepository;

    use crate::SqliteAdapter;

    async fn fresh_adapter() -> SqliteAdapter {
        let adapter = SqliteAdapter::connect(":memory:").await.unwrap();
        adapter.migrate().await.unwrap();
        adapter
    }

    fn sample_envelope() -> Envelope {
        Envelope {
            idempotency_key: None,
            subject: None,
            sender: "sender@example.com".to_owned(),
            recipients: vec![],
            body: BodySource::Plain(Plain::try_new(Some("hello".to_owned()), None).unwrap()),
            variables: serde_json::Map::new(),
        }
    }

    #[tokio::test]
    async fn save_inserts_a_row() {
        let adapter = fresh_adapter().await;
        let id = EmailId::default();
        adapter.save(id, &sample_envelope()).await.unwrap();

        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM emails")
            .fetch_one(adapter.pool())
            .await
            .unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn save_persists_the_idempotency_key() {
        let adapter = fresh_adapter().await;
        let id = EmailId::default();
        let mut envelope = sample_envelope();
        envelope.idempotency_key = Some("abc".to_owned());
        adapter.save(id, &envelope).await.unwrap();

        let key: String = sqlx::query_scalar("SELECT idempotency_key FROM emails")
            .fetch_one(adapter.pool())
            .await
            .unwrap();
        assert_eq!(key, "abc");
    }

    #[tokio::test]
    async fn save_persists_mjml_inline_body() {
        let adapter = fresh_adapter().await;
        let id = EmailId::default();
        let mut envelope = sample_envelope();
        envelope.body = BodySource::Mjml(MjmlSource::Inline("<mjml>...</mjml>".to_owned()));
        adapter.save(id, &envelope).await.unwrap();

        let body_json: String = sqlx::query_scalar("SELECT body FROM emails")
            .fetch_one(adapter.pool())
            .await
            .unwrap();
        assert!(
            body_json.contains("\"kind\":\"mjml_inline\""),
            "body_json={body_json}"
        );
        assert!(
            body_json.contains("<mjml>...</mjml>"),
            "body_json={body_json}"
        );
    }
}
