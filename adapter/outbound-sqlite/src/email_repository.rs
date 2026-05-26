use anyhow::Context;
use catapulte_domain::entity::attachment::{AttachmentRef, BlobRef};
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
use crate::dto::{
    AttachmentRefDto, BodySourceDto, EnvelopeBodyDto, EnvelopeBodyDtoDeser, recipients_to_dto,
};

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
        let body_dto = EnvelopeBodyDto {
            source: BodySourceDto::from(&envelope.body),
            attachments: envelope
                .attachments
                .iter()
                .map(AttachmentRefDto::from)
                .collect(),
        };
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

        // rows_affected == 0 means INSERT OR IGNORE skipped the row due to a constraint
        // violation. The only UNIQUE constraint that can fire for non-null idempotency_key
        // is on idempotency_key itself, so we fetch the existing ID.
        // For null idempotency_key, SQLite treats each NULL as distinct so the UNIQUE
        // constraint cannot fire — a zero-row insert here is an unexpected primary key
        // collision.
        let Some(key) = envelope.idempotency_key.as_deref() else {
            return Err(EmailRepositoryError::Storage {
                source: anyhow::anyhow!(
                    "insert skipped with no idempotency key (unexpected id collision)"
                ),
            });
        };

        let existing_bytes: Vec<u8> =
            sqlx::query_scalar("SELECT id FROM emails WHERE idempotency_key = ?")
                .bind(key)
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
                    e.created_at_ms, \
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
            SELECT id, idempotency_key, subject, sender, recipients, created_at_ms, latest_event_type \
            FROM email_status \
            WHERE 1=1",
        );

        if let Some(id) = params.id {
            qb.push(" AND id = ");
            qb.push_bind(id.as_uuid().as_bytes().to_vec());
        }
        if let Some(after) = params.after_ms {
            qb.push(" AND created_at_ms > ");
            qb.push_bind(after);
        }
        if let Some(before) = params.before_ms {
            qb.push(" AND created_at_ms < ");
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

        qb.push(" ORDER BY created_at_ms DESC, id DESC LIMIT ");
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

    async fn set_attachments(
        &self,
        id: EmailId,
        attachments: &[AttachmentRef],
    ) -> Result<(), EmailRepositoryError> {
        use crate::dto::EnvelopeBodyDtoDeser;

        let id_bytes = id.as_uuid().as_bytes().to_vec();

        let mut tx = self
            .pool()
            .begin()
            .await
            .context("starting transaction")
            .map_err(|source| EmailRepositoryError::Storage { source })?;

        let row: Option<sqlx::types::Json<EnvelopeBodyDtoDeser>> =
            sqlx::query_scalar("SELECT body FROM emails WHERE id = ?")
                .bind(&id_bytes)
                .fetch_optional(&mut *tx)
                .await
                .context("reading body for set_attachments")
                .map_err(|source| EmailRepositoryError::Storage { source })?;

        let existing_body = row.ok_or_else(|| EmailRepositoryError::Storage {
            source: anyhow::anyhow!("email not found for set_attachments: id={}", id.as_uuid()),
        })?;

        let (source, _) = existing_body.0.split();
        let new_dtos: Vec<AttachmentRefDto> =
            attachments.iter().map(AttachmentRefDto::from).collect();
        let new_body = EnvelopeBodyDto {
            source,
            attachments: new_dtos,
        };

        let result = sqlx::query("UPDATE emails SET body = ? WHERE id = ?")
            .bind(sqlx::types::Json(&new_body))
            .bind(&id_bytes)
            .execute(&mut *tx)
            .await
            .context("writing body for set_attachments")
            .map_err(|source| EmailRepositoryError::Storage { source })?;

        if result.rows_affected() == 0 {
            return Err(EmailRepositoryError::Storage {
                source: anyhow::anyhow!("email not found for set_attachments: id={}", id.as_uuid()),
            });
        }

        tx.commit()
            .await
            .context("committing set_attachments transaction")
            .map_err(|source| EmailRepositoryError::Storage { source })?;

        Ok(())
    }

    async fn delete(&self, id: EmailId) -> Result<(), EmailRepositoryError> {
        let id_bytes = id.as_uuid().as_bytes().to_vec();
        sqlx::query("DELETE FROM emails WHERE id = ?")
            .bind(&id_bytes)
            .execute(self.pool())
            .await
            .context("deleting email")
            .map_err(|source| EmailRepositoryError::Storage { source })?;
        Ok(())
    }

    async fn list_all_attachment_blobs(&self) -> Result<Vec<BlobRef>, EmailRepositoryError> {
        let rows: Vec<String> = sqlx::query_scalar(
            "WITH email_status AS (\
                SELECT e.body, COALESCE(\
                    (SELECT event_type FROM lifecycle_events le \
                     WHERE le.email_id = e.id \
                     ORDER BY le.created_at DESC, le.id DESC LIMIT 1),\
                    'queued'\
                ) AS latest_event_type \
                FROM emails e\
            ) \
            SELECT body FROM email_status \
            WHERE latest_event_type NOT IN ('sent', 'failed')",
        )
        .fetch_all(self.pool())
        .await
        .context("listing email bodies for attachment blobs")
        .map_err(|source| EmailRepositoryError::Storage { source })?;

        let mut blobs = Vec::new();
        for body_json in rows {
            let Ok(deser) = serde_json::from_str::<EnvelopeBodyDtoDeser>(&body_json) else {
                continue;
            };
            let (_, attachments) = deser.split();
            for att in attachments {
                blobs.push(BlobRef {
                    backend: att.blob.backend,
                    key: att.blob.key,
                });
            }
        }
        Ok(blobs)
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
        let created_at_ms: i64 = row
            .try_get("created_at_ms")
            .context("reading created_at_ms")?;
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
    use catapulte_domain::entity::attachment::{AttachmentRef, BlobRef};
    use catapulte_domain::entity::body::{BodySource, MjmlSource, Plain};
    use catapulte_domain::entity::email::{EmailId, RecipientKind};
    use catapulte_domain::entity::envelope::Envelope;
    use catapulte_domain::entity::lifecycle_event::LifecycleEvent;
    use catapulte_domain::entity::sender::SenderName;
    use catapulte_domain::port::email_repository::{
        EmailRepository, EmailRepositoryError, EmailStatus, ListEmailsParams, SaveResult,
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
            attachments: vec![],
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
        adapter
            .publish(&LifecycleEvent::Sent {
                id,
                sender_name: SenderName::new("test"),
            })
            .await
            .unwrap();

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
                sender_name: Some(SenderName::new("test")),
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
            .publish(&LifecycleEvent::Sent {
                id: id3,
                sender_name: SenderName::new("test"),
            })
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
        let body_dto = crate::dto::BodySourceDto::from(&env.body);
        let recip_dto = crate::dto::recipients_to_dto(&env.recipients);

        sqlx::query(
            "INSERT INTO emails (id, sender, recipients, body, variables, created_at_ms) VALUES (?, ?, ?, ?, ?, ?)",
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
            "INSERT INTO emails (id, sender, recipients, body, variables, created_at_ms) VALUES (?, ?, ?, ?, ?, ?)",
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
    async fn save_with_duplicate_id_and_no_idempotency_key_returns_error() {
        let adapter = fresh_adapter().await;
        let id = EmailId::default();
        adapter.save(id, &sample_envelope()).await.unwrap();
        let result = adapter.save(id, &sample_envelope()).await;
        assert!(
            result.is_err(),
            "expected error on duplicate id with no idempotency key"
        );
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

    #[tokio::test]
    async fn set_attachments_persists_refs_and_preserves_body() {
        use crate::dto::EnvelopeBodyDtoDeser;

        let adapter = fresh_adapter().await;
        let id = EmailId::default();
        adapter.save(id, &sample_envelope()).await.unwrap();

        let attachments = vec![
            AttachmentRef {
                filename: "invoice.pdf".to_owned(),
                content_type: "application/pdf".to_owned(),
                size_bytes: 1024,
                blob: BlobRef {
                    backend: "s3".to_owned(),
                    key: "uploads/invoice.pdf".to_owned(),
                },
            },
            AttachmentRef {
                filename: "photo.png".to_owned(),
                content_type: "image/png".to_owned(),
                size_bytes: 2048,
                blob: BlobRef {
                    backend: "gcs".to_owned(),
                    key: "media/photo.png".to_owned(),
                },
            },
        ];
        adapter.set_attachments(id, &attachments).await.unwrap();

        let body_json: String = sqlx::query_scalar("SELECT body FROM emails")
            .fetch_one(adapter.pool())
            .await
            .unwrap();

        let deser: EnvelopeBodyDtoDeser = serde_json::from_str(&body_json).unwrap();
        let (source_dto, attachment_dtos) = deser.split();

        let body = BodySource::try_from(source_dto).unwrap();
        assert!(matches!(body, BodySource::Plain(_)));

        assert_eq!(attachment_dtos.len(), 2);
        assert_eq!(attachment_dtos[0].filename, "invoice.pdf");
        assert_eq!(attachment_dtos[0].blob.backend, "s3");
        assert_eq!(attachment_dtos[1].filename, "photo.png");
        assert_eq!(attachment_dtos[1].blob.backend, "gcs");
    }

    #[tokio::test]
    async fn set_attachments_on_missing_id_returns_storage_error() {
        let adapter = fresh_adapter().await;
        let id = EmailId::default();
        let result = adapter.set_attachments(id, &[]).await;
        assert!(
            matches!(result, Err(EmailRepositoryError::Storage { .. })),
            "expected Storage error for missing id"
        );
    }

    #[tokio::test]
    async fn set_attachments_normalizes_legacy_body_shape() {
        use crate::dto::EnvelopeBodyDtoDeser;

        let adapter = fresh_adapter().await;
        let id = EmailId::default();
        let id_bytes = id.as_uuid().as_bytes().to_vec();

        // Insert a row with a legacy bare-BodySourceDto JSON, bypassing save().
        sqlx::query(
            "INSERT INTO emails (id, sender, recipients, body, variables) \
             VALUES (?, 'sender@example.com', '[]', '{\"kind\":\"plain\",\"text\":\"hi\",\"html\":null}', '{}')",
        )
        .bind(&id_bytes)
        .execute(adapter.pool())
        .await
        .unwrap();

        let attachments = vec![AttachmentRef {
            filename: "doc.pdf".to_owned(),
            content_type: "application/pdf".to_owned(),
            size_bytes: 512,
            blob: BlobRef {
                backend: "s3".to_owned(),
                key: "docs/doc.pdf".to_owned(),
            },
        }];
        adapter.set_attachments(id, &attachments).await.unwrap();

        let body_json: String = sqlx::query_scalar("SELECT body FROM emails WHERE id = ?")
            .bind(&id_bytes)
            .fetch_one(adapter.pool())
            .await
            .unwrap();

        // Check wrapped shape: has "source", no top-level "kind".
        let value: serde_json::Value = serde_json::from_str(&body_json).unwrap();
        assert!(
            value.get("source").is_some(),
            "expected top-level 'source' key, got: {value}"
        );
        assert!(
            value.get("kind").is_none(),
            "expected no top-level 'kind' key (legacy shape), got: {value}"
        );

        // Check that deserialization produces the original body and the new attachments.
        let deser: EnvelopeBodyDtoDeser = serde_json::from_str(&body_json).unwrap();
        let (source_dto, attachment_dtos) = deser.split();

        let body = BodySource::try_from(source_dto).unwrap();
        match body {
            BodySource::Plain(ref p) => {
                assert_eq!(p.text(), Some("hi"));
                assert_eq!(p.html(), None);
            }
            BodySource::Mjml(_) => panic!("expected Plain body, got {body:?}"),
        }
        assert_eq!(attachment_dtos.len(), 1);
        assert_eq!(attachment_dtos[0].filename, "doc.pdf");
    }

    #[tokio::test]
    async fn delete_removes_the_row() {
        let adapter = fresh_adapter().await;
        let id = EmailId::default();
        adapter.save(id, &sample_envelope()).await.unwrap();

        adapter.delete(id).await.unwrap();

        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM emails")
            .fetch_one(adapter.pool())
            .await
            .unwrap();
        assert_eq!(count, 0);

        // idempotent: second delete must also succeed
        adapter.delete(id).await.unwrap();
    }

    #[tokio::test]
    async fn list_all_attachment_blobs_excludes_terminal_emails() {
        let adapter = fresh_adapter().await;

        // No emails yet.
        let blobs = adapter.list_all_attachment_blobs().await.unwrap();
        assert!(blobs.is_empty(), "expected no blobs for empty db");

        // Email 1: queued, no attachments.
        let id1 = EmailId::default();
        adapter.save(id1, &sample_envelope()).await.unwrap();

        // Email 2: queued, two attachments.
        let id2 = EmailId::default();
        adapter.save(id2, &sample_envelope()).await.unwrap();
        adapter
            .set_attachments(
                id2,
                &[
                    AttachmentRef {
                        filename: "a.pdf".into(),
                        content_type: "application/pdf".into(),
                        size_bytes: 1,
                        blob: BlobRef {
                            backend: "fs".into(),
                            key: "key-a".into(),
                        },
                    },
                    AttachmentRef {
                        filename: "b.pdf".into(),
                        content_type: "application/pdf".into(),
                        size_bytes: 2,
                        blob: BlobRef {
                            backend: "fs".into(),
                            key: "key-b".into(),
                        },
                    },
                ],
            )
            .await
            .unwrap();

        // Email 3: marked Sent — its blob must NOT appear in GC liveness.
        let id3 = EmailId::default();
        adapter.save(id3, &sample_envelope()).await.unwrap();
        adapter
            .set_attachments(
                id3,
                &[AttachmentRef {
                    filename: "sent.pdf".into(),
                    content_type: "application/pdf".into(),
                    size_bytes: 3,
                    blob: BlobRef {
                        backend: "fs".into(),
                        key: "key-sent".into(),
                    },
                }],
            )
            .await
            .unwrap();
        adapter
            .publish(&LifecycleEvent::Sent {
                id: id3,
                sender_name: catapulte_domain::entity::sender::SenderName::new("test"),
            })
            .await
            .unwrap();

        // Email 4: marked Failed — its blob must NOT appear in GC liveness.
        let id4 = EmailId::default();
        adapter.save(id4, &sample_envelope()).await.unwrap();
        adapter
            .set_attachments(
                id4,
                &[AttachmentRef {
                    filename: "failed.pdf".into(),
                    content_type: "application/pdf".into(),
                    size_bytes: 4,
                    blob: BlobRef {
                        backend: "fs".into(),
                        key: "key-failed".into(),
                    },
                }],
            )
            .await
            .unwrap();
        adapter
            .publish(&LifecycleEvent::Failed {
                id: id4,
                reason: "err".into(),
                sender_name: None,
            })
            .await
            .unwrap();

        // Only the two blobs from the queued email (id2) must be returned.
        let mut blobs = adapter.list_all_attachment_blobs().await.unwrap();
        blobs.sort_by(|a, b| a.key.cmp(&b.key));
        assert_eq!(blobs.len(), 2, "sent and failed emails must be excluded");
        assert_eq!(blobs[0].key, "key-a");
        assert_eq!(blobs[1].key, "key-b");
    }
}
