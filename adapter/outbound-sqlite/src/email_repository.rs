use anyhow::Context;
use catapulte_domain::entity::email::EmailId;
use catapulte_domain::entity::envelope::Envelope;
use catapulte_domain::port::email_repository::{
    EmailRecord, EmailRepository, EmailRepositoryError, EmailStatus, ListEmailsParams, SaveResult,
};
use sqlx::QueryBuilder;
use sqlx::Row;
use sqlx::Sqlite;
use sqlx::types::Json;

use crate::SqliteAdapter;
use crate::dto::{BodySourceDto, recipients_to_dto};

impl EmailRepository for SqliteAdapter {
    /// # Errors
    ///
    /// Returns `EmailRepositoryError::Storage` when the database insert fails.
    async fn save(
        &self,
        id: EmailId,
        envelope: &Envelope,
    ) -> Result<SaveResult, EmailRepositoryError> {
        let id_bytes = id.as_uuid().as_bytes().to_vec();
        let body_dto = BodySourceDto::from(&envelope.body);
        let recipients_dto = recipients_to_dto(&envelope.recipients);

        let result = sqlx::query(
            "INSERT OR IGNORE INTO emails (id, idempotency_key, subject, sender, recipients, body, variables) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&id_bytes)
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

        if result.rows_affected() == 1 {
            return Ok(SaveResult::Created(id));
        }

        // Row already exists (duplicate idempotency_key); fetch the existing ID.
        let existing_bytes: Vec<u8> =
            sqlx::query_scalar("SELECT id FROM emails WHERE idempotency_key = ?")
                .bind(envelope.idempotency_key.as_deref())
                .fetch_one(self.pool())
                .await
                .context("fetching existing email by idempotency key")
                .map_err(|source| EmailRepositoryError::Storage { source })?;

        let existing_uuid = uuid::Uuid::from_slice(&existing_bytes)
            .context("parsing existing email id")
            .map_err(|source| EmailRepositoryError::Storage { source })?;
        Ok(SaveResult::Duplicate(EmailId::from(existing_uuid)))
    }

    async fn list_emails(
        &self,
        params: ListEmailsParams,
    ) -> Result<Vec<EmailRecord>, EmailRepositoryError> {
        let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new(
            "WITH email_status AS (\
                SELECT \
                    e.id, \
                    e.idempotency_key, \
                    e.subject, \
                    e.sender, \
                    e.recipients, \
                    e.created_at, \
                    COALESCE(\
                        (SELECT le.event_type \
                         FROM lifecycle_events le \
                         WHERE le.email_id = e.id \
                         ORDER BY le.created_at DESC, le.id DESC \
                         LIMIT 1),\
                        'queued'\
                    ) AS latest_event_type \
                FROM emails e\
            ) \
            SELECT id, idempotency_key, subject, sender, recipients, created_at, latest_event_type \
            FROM email_status \
            WHERE 1=1",
        );

        if let Some(id) = params.id {
            qb.push(" AND id = ");
            qb.push_bind(id.as_uuid().as_bytes().to_vec());
        }
        if let Some(after) = params.after_ms {
            qb.push(" AND created_at > ");
            qb.push_bind(after);
        }
        if let Some(before) = params.before_ms {
            qb.push(" AND created_at < ");
            qb.push_bind(before);
        }
        if let Some(recipient) = params.recipient {
            qb.push(
                " AND EXISTS (SELECT 1 FROM json_each(recipients) WHERE json_extract(json_each.value, '$.address') LIKE '%' || ",
            );
            qb.push_bind(recipient);
            qb.push(" || '%')");
        }
        match params.status {
            Some(EmailStatus::Sent) => {
                qb.push(" AND latest_event_type = ");
                qb.push_bind("sent");
            }
            Some(EmailStatus::Failed) => {
                qb.push(" AND latest_event_type = ");
                qb.push_bind("failed");
            }
            Some(EmailStatus::Queued) => {
                qb.push(" AND latest_event_type NOT IN ('sent', 'failed')");
            }
            None => {}
        }

        qb.push(" ORDER BY created_at DESC, id DESC LIMIT ");
        qb.push_bind(i64::from(params.limit));
        qb.push(" OFFSET ");
        qb.push_bind(i64::from(params.offset));

        let rows = qb
            .build()
            .fetch_all(self.pool())
            .await
            .context("listing emails")
            .map_err(|source| EmailRepositoryError::Storage { source })?;

        rows.iter()
            .map(SqliteAdapter::row_to_email_record)
            .collect::<anyhow::Result<Vec<_>>>()
            .map_err(|source| EmailRepositoryError::Storage { source })
    }
}

impl SqliteAdapter {
    fn row_to_email_record(row: &sqlx::sqlite::SqliteRow) -> anyhow::Result<EmailRecord> {
        let id_bytes: Vec<u8> = row.try_get("id").context("reading id")?;
        let id = uuid::Uuid::from_slice(&id_bytes).context("parsing id")?;
        let idempotency_key: Option<String> = row
            .try_get("idempotency_key")
            .context("reading idempotency_key")?;
        let subject: Option<String> = row.try_get("subject").context("reading subject")?;
        let sender: String = row.try_get("sender").context("reading sender")?;
        let recipients_json: sqlx::types::Json<Vec<crate::dto::RecipientDto>> =
            row.try_get("recipients").context("reading recipients")?;
        let created_at_ms: i64 = row.try_get("created_at").context("reading created_at")?;
        let latest_event_type: String = row
            .try_get("latest_event_type")
            .context("reading latest_event_type")?;
        let status = match latest_event_type.as_str() {
            "sent" => EmailStatus::Sent,
            "failed" => EmailStatus::Failed,
            _ => EmailStatus::Queued,
        };
        Ok(EmailRecord {
            id: EmailId::from(id),
            idempotency_key,
            subject,
            sender,
            recipients: crate::dto::recipients_from_dto(recipients_json.0),
            created_at_ms,
            status,
        })
    }
}

#[cfg(test)]
mod tests {
    use catapulte_domain::entity::body::{BodySource, MjmlSource, Plain};
    use catapulte_domain::entity::email::{EmailId, RecipientKind};
    use catapulte_domain::entity::envelope::Envelope;
    use catapulte_domain::entity::lifecycle_event::LifecycleEvent;
    use catapulte_domain::port::email_repository::{
        EmailRepository, EmailStatus, ListEmailsParams, SaveResult,
    };
    use catapulte_domain::port::event_publisher::EventPublisher;

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

    #[tokio::test]
    async fn save_with_same_idempotency_key_returns_duplicate() {
        let adapter = fresh_adapter().await;
        let id1 = EmailId::default();
        let mut envelope = sample_envelope();
        envelope.idempotency_key = Some("key-abc".to_owned());

        let result1 = adapter.save(id1, &envelope).await.unwrap();
        assert!(matches!(result1, SaveResult::Created(_)));

        let id2 = EmailId::default();
        let result2 = adapter.save(id2, &envelope).await.unwrap();
        match result2 {
            SaveResult::Duplicate(existing_id) => assert_eq!(existing_id, id1),
            SaveResult::Created(_) => panic!("expected Duplicate, got Created"),
        }
    }

    #[tokio::test]
    async fn save_without_idempotency_key_always_inserts() {
        let adapter = fresh_adapter().await;
        let envelope = sample_envelope(); // idempotency_key: None

        let r1 = adapter.save(EmailId::default(), &envelope).await.unwrap();
        let r2 = adapter.save(EmailId::default(), &envelope).await.unwrap();
        assert!(matches!(r1, SaveResult::Created(_)));
        assert!(matches!(r2, SaveResult::Created(_)));

        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM emails")
            .fetch_one(adapter.pool())
            .await
            .unwrap();
        assert_eq!(count, 2);
    }

    fn default_list_params() -> ListEmailsParams {
        ListEmailsParams {
            status: None,
            after_ms: None,
            before_ms: None,
            recipient: None,
            id: None,
            limit: 20,
            offset: 0,
        }
    }

    #[tokio::test]
    async fn list_emails_returns_empty_when_no_emails() {
        let adapter = fresh_adapter().await;
        let emails = adapter.list_emails(default_list_params()).await.unwrap();
        assert!(emails.is_empty());
    }

    #[tokio::test]
    async fn list_emails_status_defaults_to_queued_when_no_events() {
        let adapter = fresh_adapter().await;
        let id = EmailId::default();
        adapter.save(id, &sample_envelope()).await.unwrap();

        let emails = adapter.list_emails(default_list_params()).await.unwrap();
        assert_eq!(emails.len(), 1);
        assert_eq!(emails[0].status, EmailStatus::Queued);
    }

    #[tokio::test]
    async fn list_emails_status_sent_for_latest_event_sent() {
        let adapter = fresh_adapter().await;
        let id = EmailId::default();
        adapter.save(id, &sample_envelope()).await.unwrap();
        adapter
            .publish(&LifecycleEvent::Queued { id })
            .await
            .unwrap();
        adapter.publish(&LifecycleEvent::Sent { id }).await.unwrap();

        let emails = adapter
            .list_emails(ListEmailsParams {
                status: Some(EmailStatus::Sent),
                ..default_list_params()
            })
            .await
            .unwrap();
        assert_eq!(emails.len(), 1);
    }

    #[tokio::test]
    async fn list_emails_status_failed_filter() {
        let adapter = fresh_adapter().await;
        let id = EmailId::default();
        adapter.save(id, &sample_envelope()).await.unwrap();
        adapter
            .publish(&LifecycleEvent::Failed {
                id,
                reason: "err".into(),
            })
            .await
            .unwrap();

        let emails = adapter
            .list_emails(ListEmailsParams {
                status: Some(EmailStatus::Failed),
                ..default_list_params()
            })
            .await
            .unwrap();
        assert_eq!(emails.len(), 1);
    }

    #[tokio::test]
    async fn list_emails_status_queued_filter_includes_no_events_and_processing() {
        let adapter = fresh_adapter().await;

        // First: no events
        let id1 = EmailId::default();
        adapter.save(id1, &sample_envelope()).await.unwrap();

        // Second: only Queued event
        let id2 = EmailId::default();
        adapter.save(id2, &sample_envelope()).await.unwrap();
        adapter
            .publish(&LifecycleEvent::Queued { id: id2 })
            .await
            .unwrap();

        // Third: Sent event
        let id3 = EmailId::default();
        adapter.save(id3, &sample_envelope()).await.unwrap();
        adapter
            .publish(&LifecycleEvent::Sent { id: id3 })
            .await
            .unwrap();

        let emails = adapter
            .list_emails(ListEmailsParams {
                status: Some(EmailStatus::Queued),
                ..default_list_params()
            })
            .await
            .unwrap();
        assert_eq!(emails.len(), 2);
    }

    #[tokio::test]
    async fn list_emails_filters_by_id() {
        let adapter = fresh_adapter().await;
        let id1 = EmailId::default();
        let id2 = EmailId::default();
        adapter.save(id1, &sample_envelope()).await.unwrap();
        adapter.save(id2, &sample_envelope()).await.unwrap();

        let emails = adapter
            .list_emails(ListEmailsParams {
                id: Some(id1),
                ..default_list_params()
            })
            .await
            .unwrap();
        assert_eq!(emails.len(), 1);
        assert_eq!(emails[0].id, id1);
    }

    #[tokio::test]
    async fn list_emails_filters_by_recipient() {
        let adapter = fresh_adapter().await;
        let id = EmailId::default();
        let mut envelope = sample_envelope();
        envelope.recipients = vec![(RecipientKind::To, "alice@example.com".into())];
        adapter.save(id, &envelope).await.unwrap();

        let emails_alice = adapter
            .list_emails(ListEmailsParams {
                recipient: Some("alice".into()),
                ..default_list_params()
            })
            .await
            .unwrap();
        assert_eq!(emails_alice.len(), 1);

        let emails_bob = adapter
            .list_emails(ListEmailsParams {
                recipient: Some("bob".into()),
                ..default_list_params()
            })
            .await
            .unwrap();
        assert_eq!(emails_bob.len(), 0);
    }

    #[tokio::test]
    async fn list_emails_filters_by_created_at_range() {
        let adapter = fresh_adapter().await;
        let id1_bytes = uuid::Uuid::now_v7().as_bytes().to_vec();
        let id2_bytes = uuid::Uuid::now_v7().as_bytes().to_vec();
        let env = sample_envelope();
        let body_dto = catapulte_outbound_sqlite::dto::BodySourceDto::from(&env.body);
        let recip_dto = catapulte_outbound_sqlite::dto::recipients_to_dto(&env.recipients);

        sqlx::query(
            "INSERT INTO emails (id, sender, recipients, body, variables, created_at) VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(&id1_bytes)
        .bind(&env.sender)
        .bind(sqlx::types::Json(&recip_dto))
        .bind(sqlx::types::Json(&body_dto))
        .bind(sqlx::types::Json(&env.variables))
        .bind(1000i64)
        .execute(adapter.pool())
        .await
        .unwrap();

        sqlx::query(
            "INSERT INTO emails (id, sender, recipients, body, variables, created_at) VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(&id2_bytes)
        .bind(&env.sender)
        .bind(sqlx::types::Json(&recip_dto))
        .bind(sqlx::types::Json(&body_dto))
        .bind(sqlx::types::Json(&env.variables))
        .bind(2000i64)
        .execute(adapter.pool())
        .await
        .unwrap();

        let emails = adapter
            .list_emails(ListEmailsParams {
                after_ms: Some(500),
                before_ms: Some(1500),
                ..default_list_params()
            })
            .await
            .unwrap();
        assert_eq!(emails.len(), 1);
        assert_eq!(emails[0].created_at_ms, 1000);
    }

    #[tokio::test]
    async fn list_emails_orders_by_created_at_desc() {
        let adapter = fresh_adapter().await;
        let id1 = EmailId::default();
        let id2 = EmailId::default();
        adapter.save(id1, &sample_envelope()).await.unwrap();
        adapter.save(id2, &sample_envelope()).await.unwrap();

        let emails = adapter.list_emails(default_list_params()).await.unwrap();
        assert!(emails.len() >= 2);
        assert!(emails[0].created_at_ms >= emails[1].created_at_ms);
    }

    #[tokio::test]
    async fn list_emails_respects_limit_and_offset() {
        let adapter = fresh_adapter().await;
        for _ in 0..5 {
            adapter
                .save(EmailId::default(), &sample_envelope())
                .await
                .unwrap();
        }

        let page1 = adapter
            .list_emails(ListEmailsParams {
                limit: 2,
                offset: 0,
                ..default_list_params()
            })
            .await
            .unwrap();
        assert_eq!(page1.len(), 2);

        let page2 = adapter
            .list_emails(ListEmailsParams {
                limit: 2,
                offset: 2,
                ..default_list_params()
            })
            .await
            .unwrap();
        assert_eq!(page2.len(), 2);

        assert_ne!(page1[0].id, page2[0].id);
        assert_ne!(page1[1].id, page2[1].id);
    }
}
