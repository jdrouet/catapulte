use anyhow::Context;
use catapulte_domain::entity::attachment::{AttachmentRef, BlobRef};
use catapulte_domain::entity::email::EmailId;
use catapulte_domain::entity::envelope::Envelope;
use catapulte_domain::entity::lifecycle_event::{TERMINAL_EVENT_TYPES, WORKER_EVENT_TYPES};
use catapulte_domain::port::email_repository::{
    DuplicateState, EmailRecord, EmailRepository, EmailRepositoryError, EmailStatus, EnqueueState,
    ListEmailsParams, SaveResult,
};
use sqlx::QueryBuilder;
use sqlx::Row;
use sqlx::types::Json;

use crate::PostgresAdapter;
use crate::dto::{
    AttachmentRefDto, BodySourceDto, EnvelopeBodyDto, EnvelopeBodyDtoDeser, EnvelopeExtrasDto,
    UnsubscribeDto, recipients_to_dto,
};

fn terminal_types_sql() -> String {
    TERMINAL_EVENT_TYPES
        .iter()
        .map(|t| format!("'{t}'"))
        .collect::<Vec<_>>()
        .join(", ")
}

impl EmailRepository for PostgresAdapter {
    /// # Errors
    ///
    /// Returns `EmailRepositoryError::Storage` when the database insert fails.
    async fn save(
        &self,
        id: EmailId,
        envelope: &Envelope,
    ) -> Result<SaveResult, EmailRepositoryError> {
        let id_uuid = id.as_uuid();
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
            "INSERT INTO emails (id, api_key, idempotency_key, correlation_id, subject, sender, recipients, body, variables, extras, enqueue_state) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, 'reserved') \
             ON CONFLICT (api_key, idempotency_key) WHERE idempotency_key IS NOT NULL DO NOTHING",
        )
        .bind(id_uuid)
        .bind(&envelope.owner)
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

        let Some(key) = envelope.idempotency_key.as_deref() else {
            return Err(EmailRepositoryError::Storage {
                source: anyhow::anyhow!(
                    "insert skipped with no idempotency key (unexpected id collision)"
                ),
            });
        };

        self.resolve_duplicate(&envelope.owner, key).await
    }

    async fn list_emails(
        &self,
        params: ListEmailsParams,
    ) -> Result<Vec<EmailRecord>, EmailRepositoryError> {
        let mut qb: QueryBuilder<sqlx::Postgres> = QueryBuilder::new(format!(
            "WITH email_status AS (\
                SELECT \
                    e.id, \
                    e.api_key, \
                    e.idempotency_key, \
                    e.subject, \
                    e.sender, \
                    e.recipients, \
                    e.created_at, \
                    COALESCE(\
                        (SELECT le.event_type \
                         FROM lifecycle_events le \
                         WHERE le.email_id = e.id \
                           AND le.event_type IN ({terminal}) \
                         ORDER BY le.created_at DESC, le.id DESC \
                         LIMIT 1),\
                        'queued'\
                    ) AS latest_terminal_event_type \
                FROM emails e\
            ) \
            SELECT id, idempotency_key, subject, sender, recipients, created_at, latest_terminal_event_type \
            FROM email_status \
            WHERE 1=1",
            terminal = terminal_types_sql(),
        ));

        if let Some(id) = params.id {
            qb.push(" AND id = ");
            qb.push_bind(id.as_uuid());
        }
        if let Some(owner) = params.owner {
            qb.push(" AND api_key = ");
            qb.push_bind(owner);
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
                " AND EXISTS (SELECT 1 FROM jsonb_array_elements(recipients) r WHERE COALESCE(r->>'bare', r->>'address') ILIKE '%' || ",
            );
            qb.push_bind(recipient);
            qb.push(" || '%')");
        }
        if let Some(template) = params.template {
            qb.push(" AND id IN (SELECT id FROM emails WHERE body->'source'->>'kind' = 'mjml_named' AND body->'source'->>'name' = ");
            qb.push_bind(template);
            qb.push(")");
        }
        match params.status {
            Some(EmailStatus::Sent) => {
                qb.push(" AND latest_terminal_event_type = ");
                qb.push_bind("delivery.succeeded");
            }
            Some(EmailStatus::Failed) => {
                qb.push(" AND latest_terminal_event_type = ");
                qb.push_bind("delivery.failed");
            }
            Some(EmailStatus::Queued) => {
                qb.push(format!(
                    " AND latest_terminal_event_type NOT IN ({})",
                    terminal_types_sql()
                ));
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
            .map(PostgresAdapter::row_to_email_record)
            .collect::<anyhow::Result<Vec<_>>>()
            .map_err(|source| EmailRepositoryError::Storage { source })
    }

    async fn set_enqueue_state(
        &self,
        id: EmailId,
        owner: &str,
        state: EnqueueState,
    ) -> Result<(), EmailRepositoryError> {
        let value = match state {
            EnqueueState::Publishable => "publishable",
            EnqueueState::Enqueued => "enqueued",
        };
        let result =
            sqlx::query("UPDATE emails SET enqueue_state = $1 WHERE id = $2 AND api_key = $3")
                .bind(value)
                .bind(id.as_uuid())
                .bind(owner)
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

        let id_uuid = id.as_uuid();

        let mut tx = self
            .pool()
            .begin()
            .await
            .context("starting transaction")
            .map_err(|source| EmailRepositoryError::Storage { source })?;

        let row: Option<sqlx::types::Json<EnvelopeBodyDtoDeser>> =
            sqlx::query_scalar("SELECT body FROM emails WHERE id = $1 FOR UPDATE")
                .bind(id_uuid)
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

        let result = sqlx::query("UPDATE emails SET body = $1 WHERE id = $2")
            .bind(sqlx::types::Json(&new_body))
            .bind(id_uuid)
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
        let id_uuid = id.as_uuid();
        sqlx::query("DELETE FROM emails WHERE id = $1")
            .bind(id_uuid)
            .execute(self.pool())
            .await
            .context("deleting email")
            .map_err(|source| EmailRepositoryError::Storage { source })?;
        Ok(())
    }

    async fn list_all_attachment_blobs(&self) -> Result<Vec<BlobRef>, EmailRepositoryError> {
        // Unlike status, any later event (e.g. a redelivered `sending`) revives an email: a
        // duplicate copy can retry after a terminal event, so its blobs must outlive it.
        let rows: Vec<serde_json::Value> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
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
            WHERE latest_event_type NOT IN ({terminal})",
            terminal = terminal_types_sql(),
        )))
        .fetch_all(self.pool())
        .await
        .context("listing email bodies for attachment blobs")
        .map_err(|source| EmailRepositoryError::Storage { source })?;

        let mut blobs = Vec::new();
        for body_value in rows {
            let Ok(deser) = serde_json::from_value::<EnvelopeBodyDtoDeser>(body_value) else {
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

impl PostgresAdapter {
    fn row_to_email_record(row: &sqlx::postgres::PgRow) -> anyhow::Result<EmailRecord> {
        let id: uuid::Uuid = row.try_get("id").context("reading id")?;
        let idempotency_key: Option<String> = row
            .try_get("idempotency_key")
            .context("reading idempotency_key")?;
        let subject: Option<String> = row.try_get("subject").context("reading subject")?;
        let sender: String = row.try_get("sender").context("reading sender")?;
        let recipients_json: sqlx::types::Json<Vec<crate::dto::RecipientDto>> =
            row.try_get("recipients").context("reading recipients")?;
        let created_at_ms: i64 = row.try_get("created_at").context("reading created_at")?;
        let latest_terminal_event_type: String = row
            .try_get("latest_terminal_event_type")
            .context("reading latest_terminal_event_type")?;
        let status = match latest_terminal_event_type.as_str() {
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

impl PostgresAdapter {
    /// Looks the existing row up in one transaction, locking it with `FOR UPDATE` so
    /// concurrent same-key recoveries serialise. A `publishable` row with worker evidence is
    /// promoted to `enqueued`; without evidence its envelope is returned for replay.
    async fn resolve_duplicate(
        &self,
        owner: &str,
        key: &str,
    ) -> Result<SaveResult, EmailRepositoryError> {
        let storage = |source| EmailRepositoryError::Storage { source };
        let mut tx = self
            .pool()
            .begin()
            .await
            .context("starting duplicate lookup transaction")
            .map_err(storage)?;

        let row = sqlx::query(
            "SELECT id, enqueue_state, api_key, idempotency_key, correlation_id, subject, sender, \
                    recipients, body, variables, extras \
             FROM emails WHERE api_key = $1 AND idempotency_key = $2 FOR UPDATE",
        )
        .bind(owner)
        .bind(key)
        .fetch_one(&mut *tx)
        .await
        .context("fetching existing email by idempotency key")
        .map_err(storage)?;

        let existing_uuid: uuid::Uuid = row.try_get("id").map_err(|e| storage(e.into()))?;
        let state: String = row
            .try_get("enqueue_state")
            .map_err(|e| storage(e.into()))?;

        let (state, recovery) = match state.as_str() {
            "reserved" => (DuplicateState::Reserved, None),
            "enqueued" => (DuplicateState::Complete, None),
            "publishable" => {
                let has_evidence: bool = sqlx::query_scalar(
                    "SELECT EXISTS(SELECT 1 FROM lifecycle_events \
                     WHERE email_id = $1 AND event_type = ANY($2))",
                )
                .bind(existing_uuid)
                .bind(WORKER_EVENT_TYPES)
                .fetch_one(&mut *tx)
                .await
                .context("checking worker evidence")
                .map_err(storage)?;
                if has_evidence {
                    sqlx::query(
                        "UPDATE emails SET enqueue_state = 'enqueued' WHERE id = $1 AND api_key = $2",
                    )
                    .bind(existing_uuid)
                    .bind(owner)
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
            id: EmailId::from(existing_uuid),
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

    async fn fresh_adapter() -> (
        PostgresAdapter,
        testcontainers::ContainerAsync<testcontainers::GenericImage>,
    ) {
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
        (adapter, pg)
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
        let (adapter, _container) = fresh_adapter().await;
        let id = EmailId::default();
        adapter.save(id, &sample_envelope()).await.unwrap();

        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM emails")
            .fetch_one(adapter.pool())
            .await
            .unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn save_with_same_idempotency_key_returns_duplicate() {
        let (adapter, _container) = fresh_adapter().await;
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

    async fn keyed_row(adapter: &PostgresAdapter, state: Option<EnqueueState>) -> EmailId {
        let id = EmailId::default();
        let mut envelope = sample_envelope();
        envelope.idempotency_key = Some("key-abc".to_owned());
        envelope.sender = "stored@example.com".to_owned();
        adapter.save(id, &envelope).await.unwrap();
        if let Some(state) = state {
            adapter.set_enqueue_state(id, "", state).await.unwrap();
        }
        id
    }

    async fn add_event(adapter: &PostgresAdapter, id: EmailId, event_type: &str) {
        sqlx::query("INSERT INTO lifecycle_events (id, email_id, event_type) VALUES ($1, $2, $3)")
            .bind(uuid::Uuid::now_v7())
            .bind(id.as_uuid())
            .bind(event_type)
            .execute(adapter.pool())
            .await
            .unwrap();
    }

    async fn state_of(adapter: &PostgresAdapter, id: EmailId) -> String {
        sqlx::query_scalar("SELECT enqueue_state FROM emails WHERE id = $1")
            .bind(id.as_uuid())
            .fetch_one(adapter.pool())
            .await
            .unwrap()
    }

    async fn duplicate(adapter: &PostgresAdapter) -> (EmailId, DuplicateState, Option<Envelope>) {
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
        let (adapter, _container) = fresh_adapter().await;
        let id = keyed_row(&adapter, Some(EnqueueState::Publishable)).await;

        let (found, _, recovery) = duplicate(&adapter).await;

        assert_eq!(found, id);
        assert_eq!(recovery.unwrap().sender, "stored@example.com");
        assert_eq!(state_of(&adapter, id).await, "publishable");
    }

    #[tokio::test]
    async fn publishable_duplicate_with_only_informational_events_still_returns_the_envelope() {
        let (adapter, _container) = fresh_adapter().await;
        let id = keyed_row(&adapter, Some(EnqueueState::Publishable)).await;
        add_event(&adapter, id, "queued").await;
        add_event(&adapter, id, "suppressed").await;

        let (_, _, recovery) = duplicate(&adapter).await;

        assert!(recovery.is_some());
        assert_eq!(state_of(&adapter, id).await, "publishable");
    }

    #[tokio::test]
    async fn publishable_duplicate_with_a_worker_event_becomes_enqueued() {
        let (adapter, _container) = fresh_adapter().await;
        let id = keyed_row(&adapter, Some(EnqueueState::Publishable)).await;
        add_event(&adapter, id, "sending").await;

        let (_, state, recovery) = duplicate(&adapter).await;

        assert!(recovery.is_none());
        assert_eq!(state, DuplicateState::Complete);
        assert_eq!(state_of(&adapter, id).await, "enqueued");
    }

    #[tokio::test]
    async fn reserved_duplicate_is_never_replayed() {
        let (adapter, _container) = fresh_adapter().await;
        keyed_row(&adapter, None).await;

        let (_, state, recovery) = duplicate(&adapter).await;

        assert!(recovery.is_none());
        assert_eq!(state, DuplicateState::Reserved);
    }

    #[tokio::test]
    async fn reply_to_and_headers_survive_save_and_recovery() {
        let (adapter, _container) = fresh_adapter().await;
        let id = EmailId::default();
        let mut envelope = sample_envelope();
        envelope.idempotency_key = Some("key-abc".to_owned());
        envelope.reply_to = vec!["Support <help@example.com>".to_owned()];
        envelope.headers = vec![("X-Order-Id".to_owned(), "42".to_owned())];
        adapter.save(id, &envelope).await.unwrap();
        adapter
            .set_enqueue_state(id, "", EnqueueState::Publishable)
            .await
            .unwrap();

        let (_, _, recovery) = duplicate(&adapter).await;

        let recovered = recovery.unwrap();
        assert_eq!(recovered.reply_to, envelope.reply_to);
        assert_eq!(recovered.headers, envelope.headers);
    }

    #[tokio::test]
    async fn row_without_extras_recovers_with_empty_reply_to_and_headers() {
        let (adapter, _container) = fresh_adapter().await;
        let id = keyed_row(&adapter, Some(EnqueueState::Publishable)).await;
        sqlx::query("UPDATE emails SET extras = NULL WHERE id = $1")
            .bind(id.as_uuid())
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
        let (adapter, _container) = fresh_adapter().await;
        keyed_row(&adapter, Some(EnqueueState::Enqueued)).await;

        let (_, state, recovery) = duplicate(&adapter).await;

        assert!(recovery.is_none());
        assert_eq!(state, DuplicateState::Complete);
    }

    #[tokio::test]
    async fn save_without_idempotency_key_always_inserts() {
        let (adapter, _container) = fresh_adapter().await;
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
            owner: None,
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
        let (adapter, _container) = fresh_adapter().await;
        let emails = adapter.list_emails(default_list_params()).await.unwrap();
        assert!(emails.is_empty());
    }

    #[tokio::test]
    async fn list_emails_status_defaults_to_queued_when_no_events() {
        let (adapter, _container) = fresh_adapter().await;
        let id = EmailId::default();
        adapter.save(id, &sample_envelope()).await.unwrap();

        let emails = adapter.list_emails(default_list_params()).await.unwrap();
        assert_eq!(emails.len(), 1);
        assert_eq!(emails[0].status, EmailStatus::Queued);
    }

    #[tokio::test]
    async fn list_emails_status_sent_for_latest_event_sent() {
        let (adapter, _container) = fresh_adapter().await;
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
        let (adapter, _container) = fresh_adapter().await;
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
        let (adapter, _container) = fresh_adapter().await;

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
        let (adapter, _container) = fresh_adapter().await;
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
        let (adapter, _container) = fresh_adapter().await;
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

    async fn recipient_filter_matches(adapter: &PostgresAdapter, needle: &str) -> usize {
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
        let (adapter, _container) = fresh_adapter().await;
        let mut envelope = sample_envelope();
        envelope.recipients = vec![(RecipientKind::To, "Acme <Bob@Example.com>".into())];
        adapter.save(EmailId::default(), &envelope).await.unwrap();

        assert_eq!(recipient_filter_matches(&adapter, "bob@example").await, 1);
        assert_eq!(recipient_filter_matches(&adapter, "Acme").await, 0);
    }

    #[tokio::test]
    async fn recipient_filter_still_matches_legacy_rows_without_bare() {
        let (adapter, _container) = fresh_adapter().await;
        sqlx::query(
            "INSERT INTO emails (id, sender, recipients, body, variables) \
             VALUES ($1, 'sender@example.com', '[{\"kind\":\"to\",\"address\":\"legacy@example.com\"}]'::jsonb, \
             '{\"kind\":\"plain\",\"text\":\"hi\",\"html\":null}'::jsonb, '{}'::jsonb)",
        )
        .bind(uuid::Uuid::now_v7())
        .execute(adapter.pool())
        .await
        .unwrap();

        assert_eq!(recipient_filter_matches(&adapter, "legacy").await, 1);
        assert_eq!(recipient_filter_matches(&adapter, "nobody").await, 0);
    }

    #[tokio::test]
    async fn list_emails_filters_by_created_at_range() {
        let (adapter, _container) = fresh_adapter().await;
        let id1 = uuid::Uuid::now_v7();
        let id2 = uuid::Uuid::now_v7();
        let env = sample_envelope();
        let body_dto = crate::dto::BodySourceDto::from(&env.body);
        let recip_dto = crate::dto::recipients_to_dto(&env.recipients);

        sqlx::query(
            "INSERT INTO emails (id, sender, recipients, body, variables, created_at) VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(id1)
        .bind(&env.sender)
        .bind(sqlx::types::Json(&recip_dto))
        .bind(sqlx::types::Json(&body_dto))
        .bind(sqlx::types::Json(&env.variables))
        .bind(1000i64)
        .execute(adapter.pool())
        .await
        .unwrap();

        sqlx::query(
            "INSERT INTO emails (id, sender, recipients, body, variables, created_at) VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(id2)
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
        let (adapter, _container) = fresh_adapter().await;
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
        let (adapter, _container) = fresh_adapter().await;
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
    async fn save_with_duplicate_id_and_no_idempotency_key_returns_error() {
        let (adapter, _container) = fresh_adapter().await;
        let id = EmailId::default();
        adapter.save(id, &sample_envelope()).await.unwrap();
        let result = adapter.save(id, &sample_envelope()).await;
        assert!(
            result.is_err(),
            "expected error on duplicate id with no idempotency key"
        );
    }

    #[tokio::test]
    async fn set_attachments_persists_refs_and_preserves_body() {
        use crate::dto::EnvelopeBodyDtoDeser;

        let (adapter, _container) = fresh_adapter().await;
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

        let body_json: serde_json::Value = sqlx::query_scalar("SELECT body FROM emails")
            .fetch_one(adapter.pool())
            .await
            .unwrap();

        let deser: EnvelopeBodyDtoDeser = serde_json::from_value(body_json).unwrap();
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
        let (adapter, _container) = fresh_adapter().await;
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

        let (adapter, _container) = fresh_adapter().await;
        let id = EmailId::default();
        let id_uuid = id.as_uuid();

        // Insert a row with a legacy bare-BodySourceDto JSON, bypassing save().
        sqlx::query(
            "INSERT INTO emails (id, sender, recipients, body, variables) \
             VALUES ($1, 'sender@example.com', '[]'::jsonb, '{\"kind\":\"plain\",\"text\":\"hi\",\"html\":null}'::jsonb, '{}'::jsonb)",
        )
        .bind(id_uuid)
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

        let body_value: serde_json::Value =
            sqlx::query_scalar("SELECT body FROM emails WHERE id = $1")
                .bind(id_uuid)
                .fetch_one(adapter.pool())
                .await
                .unwrap();

        // Check wrapped shape: has "source", no top-level "kind".
        assert!(
            body_value.get("source").is_some(),
            "expected top-level 'source' key, got: {body_value}"
        );
        assert!(
            body_value.get("kind").is_none(),
            "expected no top-level 'kind' key (legacy shape), got: {body_value}"
        );

        // Check that deserialization produces the original body and the new attachments.
        let deser: EnvelopeBodyDtoDeser = serde_json::from_value(body_value).unwrap();
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
        let (adapter, _container) = fresh_adapter().await;
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
        let (adapter, _container) = fresh_adapter().await;

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
        let (adapter, _container) = fresh_adapter().await;

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
                sender_name: SenderName::new("test"),
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

    async fn add_event_at(adapter: &PostgresAdapter, id: EmailId, event_type: &str, at_ms: i64) {
        sqlx::query(
            "INSERT INTO lifecycle_events (id, email_id, event_type, created_at) VALUES ($1, $2, $3, $4)",
        )
        .bind(uuid::Uuid::now_v7())
        .bind(id.as_uuid())
        .bind(event_type)
        .bind(at_ms)
        .execute(adapter.pool())
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn status_comes_from_the_latest_terminal_event() {
        let (adapter, _container) = fresh_adapter().await;
        let mut ids = Vec::new();
        for _ in 0..5 {
            let id = EmailId::default();
            adapter.save(id, &sample_envelope()).await.unwrap();
            ids.push(id);
        }
        let [replayed, flipped, informational, none, bounced] = ids[..] else {
            unreachable!()
        };
        add_event_at(&adapter, replayed, "delivery.succeeded", 1).await;
        add_event_at(&adapter, replayed, "queued", 2).await;
        add_event_at(&adapter, flipped, "delivery.succeeded", 1).await;
        add_event_at(&adapter, flipped, "delivery.failed", 2).await;
        add_event_at(&adapter, informational, "queued", 1).await;
        add_event_at(&adapter, informational, "sending", 2).await;
        add_event_at(&adapter, bounced, "delivery.succeeded", 1).await;
        add_event_at(&adapter, bounced, "bounced", 2).await;

        let ids_with = |status| {
            let adapter = &adapter;
            async move {
                let mut found: Vec<EmailId> = adapter
                    .list_emails(ListEmailsParams {
                        status: Some(status),
                        ..default_list_params()
                    })
                    .await
                    .unwrap()
                    .into_iter()
                    .map(|e| e.id)
                    .collect();
                found.sort_by_key(EmailId::as_uuid);
                found
            }
        };
        let sorted = |mut v: Vec<EmailId>| {
            v.sort_by_key(EmailId::as_uuid);
            v
        };

        assert_eq!(
            ids_with(EmailStatus::Sent).await,
            sorted(vec![replayed, bounced])
        );
        assert_eq!(ids_with(EmailStatus::Failed).await, vec![flipped]);
        assert_eq!(
            ids_with(EmailStatus::Queued).await,
            sorted(vec![informational, none])
        );
        let all = adapter.list_emails(default_list_params()).await.unwrap();
        let status_of = |id| all.iter().find(|e| e.id == id).unwrap().status;
        assert_eq!(status_of(replayed), EmailStatus::Sent);
        assert_eq!(status_of(flipped), EmailStatus::Failed);
        assert_eq!(status_of(informational), EmailStatus::Queued);
        assert_eq!(status_of(none), EmailStatus::Queued);
        assert_eq!(status_of(bounced), EmailStatus::Sent);
    }

    async fn add_event_with_id(
        adapter: &PostgresAdapter,
        id: EmailId,
        event_type: &str,
        at_ms: i64,
        event_id: uuid::Uuid,
    ) {
        sqlx::query(
            "INSERT INTO lifecycle_events (id, email_id, event_type, created_at) VALUES ($1, $2, $3, $4)",
        )
        .bind(event_id)
        .bind(id.as_uuid())
        .bind(event_type)
        .bind(at_ms)
        .execute(adapter.pool())
        .await
        .unwrap();
    }

    async fn email_with_blob(adapter: &PostgresAdapter, key: &str) -> EmailId {
        let id = EmailId::default();
        adapter.save(id, &sample_envelope()).await.unwrap();
        adapter
            .set_attachments(
                id,
                &[AttachmentRef {
                    content_id: None,
                    filename: "a.pdf".into(),
                    content_type: "application/pdf".into(),
                    size_bytes: 1,
                    blob: BlobRef {
                        backend: "fs".into(),
                        key: key.into(),
                    },
                }],
            )
            .await
            .unwrap();
        id
    }

    #[tokio::test]
    async fn gc_liveness_follows_the_latest_event_of_any_type() {
        let (adapter, _container) = fresh_adapter().await;
        let redelivered = email_with_blob(&adapter, "redelivered").await;
        add_event_at(&adapter, redelivered, "delivery.failed", 1).await;
        add_event_at(&adapter, redelivered, "sending", 2).await;
        add_event_at(&adapter, redelivered, "retrying", 3).await;
        let done = email_with_blob(&adapter, "done").await;
        add_event_at(&adapter, done, "sending", 1).await;
        add_event_at(&adapter, done, "delivery.succeeded", 2).await;
        let replayed = email_with_blob(&adapter, "replayed").await;
        add_event_at(&adapter, replayed, "delivery.succeeded", 1).await;
        add_event_at(&adapter, replayed, "queued", 2).await;

        let mut keys: Vec<String> = adapter
            .list_all_attachment_blobs()
            .await
            .unwrap()
            .into_iter()
            .map(|b| b.key)
            .collect();
        keys.sort();
        assert_eq!(keys, ["redelivered", "replayed"]);
    }

    #[tokio::test]
    async fn status_follows_the_latest_terminal_event_and_breaks_ties_by_id() {
        let (adapter, _container) = fresh_adapter().await;
        let status_of = |id: EmailId| {
            let adapter = &adapter;
            async move {
                adapter
                    .list_emails(ListEmailsParams {
                        id: Some(id),
                        ..default_list_params()
                    })
                    .await
                    .unwrap()[0]
                    .status
            }
        };
        let low = uuid::Uuid::from_u128(1);
        let high = uuid::Uuid::from_u128(2);

        let recovered = EmailId::default();
        adapter.save(recovered, &sample_envelope()).await.unwrap();
        add_event_at(&adapter, recovered, "delivery.failed", 1).await;
        add_event_at(&adapter, recovered, "delivery.succeeded", 2).await;
        assert_eq!(status_of(recovered).await, EmailStatus::Sent);

        let tie_sent = EmailId::default();
        adapter.save(tie_sent, &sample_envelope()).await.unwrap();
        add_event_with_id(&adapter, tie_sent, "delivery.failed", 5, low).await;
        add_event_with_id(&adapter, tie_sent, "delivery.succeeded", 5, high).await;
        assert_eq!(status_of(tie_sent).await, EmailStatus::Sent);

        let low2 = uuid::Uuid::from_u128(3);
        let high2 = uuid::Uuid::from_u128(4);
        let tie_failed = EmailId::default();
        adapter.save(tie_failed, &sample_envelope()).await.unwrap();
        add_event_with_id(&adapter, tie_failed, "delivery.succeeded", 5, low2).await;
        add_event_with_id(&adapter, tie_failed, "delivery.failed", 5, high2).await;
        assert_eq!(status_of(tie_failed).await, EmailStatus::Failed);
    }

    fn owned_envelope(owner: &str, key: &str) -> Envelope {
        let mut envelope = sample_envelope();
        envelope.owner = owner.to_owned();
        envelope.idempotency_key = Some(key.to_owned());
        envelope
    }

    #[tokio::test]
    async fn same_idempotency_key_is_unique_per_owner() {
        let (adapter, _container) = fresh_adapter().await;
        let a = EmailId::default();
        let b = EmailId::default();

        let first = adapter.save(a, &owned_envelope("a", "k")).await.unwrap();
        let other = adapter.save(b, &owned_envelope("b", "k")).await.unwrap();
        let again = adapter
            .save(EmailId::default(), &owned_envelope("a", "k"))
            .await
            .unwrap();

        assert!(matches!(first, SaveResult::Created(id) if id == a));
        assert!(matches!(other, SaveResult::Created(id) if id == b));
        assert!(matches!(again, SaveResult::Duplicate { id, .. } if id == a));
        let all = adapter.list_emails(default_list_params()).await.unwrap();
        assert_eq!(all.len(), 2);
    }

    #[tokio::test]
    async fn another_owners_same_key_never_replays_or_updates_a_publishable_row() {
        let (adapter, _container) = fresh_adapter().await;
        let a = EmailId::default();
        adapter.save(a, &owned_envelope("a", "k")).await.unwrap();
        adapter
            .set_enqueue_state(a, "a", EnqueueState::Publishable)
            .await
            .unwrap();

        let b = EmailId::default();
        let result = adapter.save(b, &owned_envelope("b", "k")).await.unwrap();
        assert!(matches!(result, SaveResult::Created(id) if id == b));

        let wrong_owner = adapter
            .set_enqueue_state(a, "b", EnqueueState::Enqueued)
            .await;
        assert!(wrong_owner.is_err());
        let replay = adapter
            .save(EmailId::default(), &owned_envelope("a", "k"))
            .await
            .unwrap();
        assert!(matches!(
            replay,
            SaveResult::Duplicate { id, recovery: Some(_), .. } if id == a
        ));
    }

    #[tokio::test]
    async fn list_emails_and_events_are_scoped_to_the_owner() {
        use catapulte_domain::port::event_repository::{EventRepository, ListEventsParams};

        let (adapter, _container) = fresh_adapter().await;
        let a = EmailId::default();
        let b = EmailId::default();
        adapter.save(a, &owned_envelope("a", "ka")).await.unwrap();
        adapter.save(b, &owned_envelope("b", "kb")).await.unwrap();
        add_event(&adapter, a, "queued").await;
        add_event(&adapter, b, "queued").await;

        let emails = adapter
            .list_emails(ListEmailsParams {
                owner: Some("a".to_owned()),
                ..default_list_params()
            })
            .await
            .unwrap();
        assert_eq!(emails.iter().map(|e| e.id).collect::<Vec<_>>(), vec![a]);
        let foreign = adapter
            .list_emails(ListEmailsParams {
                owner: Some("a".to_owned()),
                id: Some(b),
                ..default_list_params()
            })
            .await
            .unwrap();
        assert!(foreign.is_empty());

        let events_params = |owner: Option<&str>, email_id: Option<EmailId>| ListEventsParams {
            owner: owner.map(str::to_owned),
            email_id,
            event_type: None,
            sender_name: None,
            error_class: None,
            after_ms: None,
            before_ms: None,
            limit: 20,
            offset: 0,
        };
        let own = adapter
            .list_events(events_params(Some("a"), None))
            .await
            .unwrap();
        assert_eq!(own.iter().map(|e| e.email_id).collect::<Vec<_>>(), vec![a]);
        let foreign = adapter
            .list_events(events_params(Some("a"), Some(b)))
            .await
            .unwrap();
        assert!(foreign.is_empty());
        let admin = adapter
            .list_events(events_params(None, None))
            .await
            .unwrap();
        assert_eq!(admin.len(), 2);
    }
}
