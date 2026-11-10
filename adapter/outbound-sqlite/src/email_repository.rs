use anyhow::Context;
use catapulte_domain::entity::attachment::{AttachmentRef, BlobRef};
use catapulte_domain::entity::email::EmailId;
use catapulte_domain::entity::envelope::Envelope;
use catapulte_domain::entity::lifecycle_event::WORKER_EVENT_TYPES;
use catapulte_domain::port::email_repository::{
    DuplicateState, EmailRecord, EmailRepository, EmailRepositoryError, EmailStatus, EnqueueState,
    ListEmailsParams, SaveResult,
};
use sqlx::QueryBuilder;
use sqlx::Row;
use sqlx::Sqlite;
use sqlx::types::Json;

use crate::SqliteAdapter;
use crate::dto::{
    AttachmentRefDto, BodySourceDto, EnvelopeBodyDto, EnvelopeBodyDtoDeser, EnvelopeExtrasDto,
    UnsubscribeDto, recipients_to_dto,
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
        let extras_dto = EnvelopeExtrasDto {
            reply_to: envelope.reply_to.clone(),
            headers: envelope.headers.clone(),
            unsubscribe: envelope.unsubscribe.as_ref().map(UnsubscribeDto::from),
        };

        let result = sqlx::query(
            "INSERT OR IGNORE INTO emails (id, idempotency_key, correlation_id, subject, sender, recipients, body, variables, extras, enqueue_state) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 'reserved')",
        )
        .bind(&id_bytes)
        .bind(envelope.idempotency_key.as_deref())
        .bind(envelope.correlation_id.as_deref())
        .bind(envelope.subject.as_deref())
        .bind(&envelope.sender)
        .bind(Json(&recipients_dto))
        .bind(Json(&body_dto))
        .bind(Json(&envelope.variables))
        .bind(Json(&extras_dto))
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

        self.resolve_duplicate(key).await
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
                " AND EXISTS (SELECT 1 FROM json_each(recipients) WHERE COALESCE(json_extract(json_each.value, '$.bare'), json_extract(json_each.value, '$.address')) LIKE '%' || ",
            );
            qb.push_bind(recipient);
            qb.push(" || '%')");
        }
        if let Some(template) = params.template {
            qb.push(" AND id IN (SELECT id FROM emails WHERE json_extract(body, '$.source.kind') = 'mjml_named' AND json_extract(body, '$.source.name') = ");
            qb.push_bind(template);
            qb.push(")");
        }
        match params.status {
            Some(EmailStatus::Sent) => {
                qb.push(" AND latest_event_type = ");
                qb.push_bind("delivery.succeeded");
            }
            Some(EmailStatus::Failed) => {
                qb.push(" AND latest_event_type = ");
                qb.push_bind("delivery.failed");
            }
            Some(EmailStatus::Queued) => {
                qb.push(" AND latest_event_type NOT IN ('delivery.succeeded', 'delivery.failed')");
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

    async fn set_enqueue_state(
        &self,
        id: EmailId,
        state: EnqueueState,
    ) -> Result<(), EmailRepositoryError> {
        let value = match state {
            EnqueueState::Publishable => "publishable",
            EnqueueState::Enqueued => "enqueued",
        };
        let result = sqlx::query("UPDATE emails SET enqueue_state = ? WHERE id = ?")
            .bind(value)
            .bind(id.as_uuid().as_bytes().to_vec())
            .execute(self.pool())
            .await
            .context("updating enqueue_state")
            .map_err(|source| EmailRepositoryError::Storage { source })?;
        if result.rows_affected() == 0 {
            return Err(EmailRepositoryError::Storage {
                source: anyhow::anyhow!(
                    "email not found for set_enqueue_state: id={}",
                    id.as_uuid()
                ),
            });
        }
        Ok(())
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
            WHERE latest_event_type NOT IN ('delivery.succeeded', 'delivery.failed')",
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
            "delivery.succeeded" => EmailStatus::Sent,
            "delivery.failed" => EmailStatus::Failed,
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

impl SqliteAdapter {
    /// Looks the existing row up in one transaction. A `publishable` row with worker
    /// evidence is promoted to `enqueued`; without evidence its envelope is returned for replay.
    async fn resolve_duplicate(&self, key: &str) -> Result<SaveResult, EmailRepositoryError> {
        let storage = |source| EmailRepositoryError::Storage { source };
        let mut tx = self
            .pool()
            .begin()
            .await
            .context("starting duplicate lookup transaction")
            .map_err(storage)?;

        let row = sqlx::query(
            "SELECT id, enqueue_state, idempotency_key, correlation_id, subject, sender, \
                    recipients, body, variables, extras \
             FROM emails WHERE idempotency_key = ?",
        )
        .bind(key)
        .fetch_one(&mut *tx)
        .await
        .context("fetching existing email by idempotency key")
        .map_err(storage)?;

        let id_bytes: Vec<u8> = row.try_get("id").map_err(|e| storage(e.into()))?;
        let existing_id = EmailId::from(
            uuid::Uuid::from_slice(&id_bytes)
                .context("parsing existing email id")
                .map_err(storage)?,
        );
        let state: String = row
            .try_get("enqueue_state")
            .map_err(|e| storage(e.into()))?;

        let (state, recovery) = match state.as_str() {
            "reserved" => (DuplicateState::Reserved, None),
            "enqueued" => (DuplicateState::Complete, None),
            "publishable" => {
                let mut evidence = QueryBuilder::<Sqlite>::new(
                    "SELECT EXISTS(SELECT 1 FROM lifecycle_events WHERE email_id = ",
                );
                evidence.push_bind(&id_bytes).push(" AND event_type IN (");
                let mut types = evidence.separated(", ");
                for t in WORKER_EVENT_TYPES {
                    types.push_bind(*t);
                }
                evidence.push("))");
                let has_evidence: bool = evidence
                    .build_query_scalar()
                    .fetch_one(&mut *tx)
                    .await
                    .context("checking worker evidence")
                    .map_err(storage)?;
                if has_evidence {
                    sqlx::query("UPDATE emails SET enqueue_state = 'enqueued' WHERE id = ?")
                        .bind(&id_bytes)
                        .execute(&mut *tx)
                        .await
                        .context("promoting enqueue_state")
                        .map_err(storage)?;
                    (DuplicateState::Complete, None)
                } else {
                    let envelope = crate::email_queue::parse_envelope(&row).map_err(storage)?;
                    (DuplicateState::Complete, Some(envelope))
                }
            }
            other => {
                return Err(storage(anyhow::anyhow!("unknown enqueue_state {other:?}")));
            }
        };

        tx.commit()
            .await
            .context("committing duplicate lookup transaction")
            .map_err(storage)?;
        Ok(SaveResult::Duplicate {
            id: existing_id,
            state,
            recovery,
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
        DuplicateState, EmailRepository, EmailRepositoryError, EmailStatus, EnqueueState,
        ListEmailsParams, SaveResult,
    };
    use catapulte_domain::port::event_publisher::EventPublisher;

    use crate::SqliteAdapter;

    async fn fresh_adapter() -> SqliteAdapter {
        let adapter = SqliteAdapter::connect(":memory:").await.unwrap();
        adapter.migrate().await.unwrap();
        adapter
    }

    fn sample_envelope() -> Envelope {
        Envelope::new(
            "sender@example.com",
            vec![],
            BodySource::Plain(Plain::try_new(Some("hello".to_owned()), None).unwrap()),
        )
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
            SaveResult::Duplicate { id, .. } => assert_eq!(id, id1),
            SaveResult::Created(_) => panic!("expected Duplicate, got Created"),
        }
    }

    async fn keyed_row(adapter: &SqliteAdapter, state: Option<EnqueueState>) -> EmailId {
        let id = EmailId::default();
        let mut envelope = sample_envelope();
        envelope.idempotency_key = Some("key-abc".to_owned());
        envelope.sender = "stored@example.com".to_owned();
        adapter.save(id, &envelope).await.unwrap();
        if let Some(state) = state {
            adapter.set_enqueue_state(id, state).await.unwrap();
        }
        id
    }

    async fn add_event(adapter: &SqliteAdapter, id: EmailId, event_type: &str) {
        sqlx::query("INSERT INTO lifecycle_events (id, email_id, event_type) VALUES (?, ?, ?)")
            .bind(EmailId::default().as_uuid().as_bytes().to_vec())
            .bind(id.as_uuid().as_bytes().to_vec())
            .bind(event_type)
            .execute(adapter.pool())
            .await
            .unwrap();
    }

    async fn state_of(adapter: &SqliteAdapter, id: EmailId) -> String {
        sqlx::query_scalar("SELECT enqueue_state FROM emails WHERE id = ?")
            .bind(id.as_uuid().as_bytes().to_vec())
            .fetch_one(adapter.pool())
            .await
            .unwrap()
    }

    async fn duplicate(adapter: &SqliteAdapter) -> (EmailId, DuplicateState, Option<Envelope>) {
        let mut envelope = sample_envelope();
        envelope.idempotency_key = Some("key-abc".to_owned());
        match adapter.save(EmailId::default(), &envelope).await.unwrap() {
            SaveResult::Duplicate {
                id,
                state,
                recovery,
            } => (id, state, recovery),
            SaveResult::Created(_) => panic!("expected Duplicate"),
        }
    }

    #[tokio::test]
    async fn publishable_duplicate_without_events_returns_the_stored_envelope() {
        let adapter = fresh_adapter().await;
        let id = keyed_row(&adapter, Some(EnqueueState::Publishable)).await;

        let (found, _, recovery) = duplicate(&adapter).await;

        assert_eq!(found, id);
        assert_eq!(recovery.unwrap().sender, "stored@example.com");
        assert_eq!(state_of(&adapter, id).await, "publishable");
    }

    #[tokio::test]
    async fn publishable_duplicate_with_only_informational_events_still_returns_the_envelope() {
        let adapter = fresh_adapter().await;
        let id = keyed_row(&adapter, Some(EnqueueState::Publishable)).await;
        add_event(&adapter, id, "queued").await;
        add_event(&adapter, id, "suppressed").await;

        let (_, _, recovery) = duplicate(&adapter).await;

        assert!(recovery.is_some());
        assert_eq!(state_of(&adapter, id).await, "publishable");
    }

    #[tokio::test]
    async fn publishable_duplicate_with_a_worker_event_becomes_enqueued() {
        let adapter = fresh_adapter().await;
        let id = keyed_row(&adapter, Some(EnqueueState::Publishable)).await;
        add_event(&adapter, id, "sending").await;

        let (_, state, recovery) = duplicate(&adapter).await;

        assert!(recovery.is_none());
        assert_eq!(state, DuplicateState::Complete);
        assert_eq!(state_of(&adapter, id).await, "enqueued");
    }

    #[tokio::test]
    async fn reserved_duplicate_is_never_replayed() {
        let adapter = fresh_adapter().await;
        keyed_row(&adapter, None).await;

        let (_, state, recovery) = duplicate(&adapter).await;

        assert!(recovery.is_none());
        assert_eq!(state, DuplicateState::Reserved);
    }

    #[tokio::test]
    async fn reply_to_and_headers_survive_save_and_recovery() {
        let adapter = fresh_adapter().await;
        let id = EmailId::default();
        let mut envelope = sample_envelope();
        envelope.idempotency_key = Some("key-abc".to_owned());
        envelope.reply_to = vec!["Support <help@example.com>".to_owned()];
        envelope.headers = vec![("X-Order-Id".to_owned(), "42".to_owned())];
        adapter.save(id, &envelope).await.unwrap();
        adapter
            .set_enqueue_state(id, EnqueueState::Publishable)
            .await
            .unwrap();

        let (_, _, recovery) = duplicate(&adapter).await;

        let recovered = recovery.unwrap();
        assert_eq!(recovered.reply_to, envelope.reply_to);
        assert_eq!(recovered.headers, envelope.headers);
    }

    #[tokio::test]
    async fn row_without_extras_recovers_with_empty_reply_to_and_headers() {
        let adapter = fresh_adapter().await;
        let id = keyed_row(&adapter, Some(EnqueueState::Publishable)).await;
        sqlx::query("UPDATE emails SET extras = NULL WHERE id = ?")
            .bind(id.as_uuid().as_bytes().to_vec())
            .execute(adapter.pool())
            .await
            .unwrap();

        let (_, _, recovery) = duplicate(&adapter).await;

        let recovered = recovery.unwrap();
        assert!(recovered.reply_to.is_empty());
        assert!(recovered.headers.is_empty());
    }

    #[tokio::test]
    async fn enqueued_duplicate_returns_no_recovery() {
        let adapter = fresh_adapter().await;
        keyed_row(&adapter, Some(EnqueueState::Enqueued)).await;

        let (_, state, recovery) = duplicate(&adapter).await;

        assert!(recovery.is_none());
        assert_eq!(state, DuplicateState::Complete);
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
            template: None,
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
            .publish(&LifecycleEvent::Queued {
                id,
                correlation_id: None,
            })
            .await
            .unwrap();
        adapter
            .publish(&LifecycleEvent::Sent {
                id,
                sender_name: SenderName::new("test"),
                correlation_id: None,
                smtp_response: None,
                message_id: "<test@example.com>".to_owned(),
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
                attempt: 3,
                reason: "err".into(),
                error_class: catapulte_domain::entity::error_class::ErrorClass::Delivery,
                sender_name: Some(SenderName::new("test")),
                correlation_id: None,
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
            .publish(&LifecycleEvent::Queued {
                id: id2,
                correlation_id: None,
            })
            .await
            .unwrap();

        // Third: Sent event
        let id3 = EmailId::default();
        adapter.save(id3, &sample_envelope()).await.unwrap();
        adapter
            .publish(&LifecycleEvent::Sent {
                id: id3,
                sender_name: SenderName::new("test"),
                correlation_id: None,
                smtp_response: None,
                message_id: "<test@example.com>".to_owned(),
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

    async fn recipient_filter_matches(adapter: &crate::SqliteAdapter, needle: &str) -> usize {
        adapter
            .list_emails(ListEmailsParams {
                recipient: Some(needle.into()),
                ..default_list_params()
            })
            .await
            .unwrap()
            .len()
    }

    #[tokio::test]
    async fn recipient_filter_matches_bare_address_not_display_name() {
        let adapter = fresh_adapter().await;
        let mut envelope = sample_envelope();
        envelope.recipients = vec![(RecipientKind::To, "Acme <Bob@Example.com>".into())];
        adapter.save(EmailId::default(), &envelope).await.unwrap();

        assert_eq!(recipient_filter_matches(&adapter, "bob@example").await, 1);
        assert_eq!(recipient_filter_matches(&adapter, "Acme").await, 0);
    }

    #[tokio::test]
    async fn recipient_filter_handles_bare_quoted_address() {
        let adapter = fresh_adapter().await;
        let mut envelope = sample_envelope();
        envelope.recipients = vec![(RecipientKind::To, "\"a b\"@example.com".into())];
        adapter.save(EmailId::default(), &envelope).await.unwrap();

        assert_eq!(recipient_filter_matches(&adapter, "a b").await, 1);
    }

    #[tokio::test]
    async fn recipient_filter_still_matches_legacy_rows_without_bare() {
        let adapter = fresh_adapter().await;
        let env = sample_envelope();
        let body_dto = crate::dto::BodySourceDto::from(&env.body);
        sqlx::query(
            "INSERT INTO emails (id, sender, recipients, body, variables) VALUES (?, ?, ?, ?, ?)",
        )
        .bind(uuid::Uuid::now_v7().as_bytes().to_vec())
        .bind(&env.sender)
        .bind(r#"[{"kind":"to","address":"legacy@example.com"}]"#)
        .bind(sqlx::types::Json(&body_dto))
        .bind(sqlx::types::Json(&env.variables))
        .execute(adapter.pool())
        .await
        .unwrap();

        assert_eq!(recipient_filter_matches(&adapter, "legacy").await, 1);
        assert_eq!(recipient_filter_matches(&adapter, "nobody").await, 0);
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
                content_id: None,
                filename: "invoice.pdf".to_owned(),
                content_type: "application/pdf".to_owned(),
                size_bytes: 1024,
                blob: BlobRef {
                    backend: "s3".to_owned(),
                    key: "uploads/invoice.pdf".to_owned(),
                },
            },
            AttachmentRef {
                content_id: None,
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
            content_id: None,
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
    async fn list_emails_template_filter_matches_only_named_mjml() {
        let adapter = fresh_adapter().await;

        // Save a named MJML email.
        let id_named = EmailId::default();
        let mut named_envelope = sample_envelope();
        named_envelope.body = BodySource::Mjml(MjmlSource::Named("welcome".to_owned()));
        adapter.save(id_named, &named_envelope).await.unwrap();

        // Save a plain email.
        let id_plain = EmailId::default();
        adapter.save(id_plain, &sample_envelope()).await.unwrap();

        // Filter by template "welcome": only the named email must be returned.
        let filtered = adapter
            .list_emails(ListEmailsParams {
                template: Some("welcome".to_owned()),
                ..default_list_params()
            })
            .await
            .unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].id, id_named);

        // No template filter: both emails are returned.
        let all = adapter.list_emails(default_list_params()).await.unwrap();
        assert_eq!(all.len(), 2);
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
                        content_id: None,
                        filename: "a.pdf".into(),
                        content_type: "application/pdf".into(),
                        size_bytes: 1,
                        blob: BlobRef {
                            backend: "fs".into(),
                            key: "key-a".into(),
                        },
                    },
                    AttachmentRef {
                        content_id: None,
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
                    content_id: None,
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
                correlation_id: None,
                smtp_response: None,
                message_id: "<test@example.com>".to_owned(),
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
                    content_id: None,
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
                attempt: 3,
                reason: "err".into(),
                error_class: catapulte_domain::entity::error_class::ErrorClass::Delivery,
                sender_name: None,
                correlation_id: None,
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
