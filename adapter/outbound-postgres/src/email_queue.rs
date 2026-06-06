use std::time::Duration;

use anyhow::Context;
use catapulte_domain::entity::body::BodySource;
use catapulte_domain::entity::email::EmailId;
use catapulte_domain::entity::envelope::Envelope;
use catapulte_domain::port::email_queue::{
    AckToken, DequeuedEmail, EmailQueue, EmailQueueError, TraceCarrier,
};

use crate::PostgresAdapter;
use crate::dto::{EnvelopeBodyDtoDeser, RecipientDto, recipients_from_dto};

fn now_ms() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(i64::MAX)
}

fn parse_envelope(row: &sqlx::postgres::PgRow) -> anyhow::Result<Envelope> {
    use sqlx::Row;
    let body_deser: sqlx::types::Json<EnvelopeBodyDtoDeser> =
        row.try_get("body").context("reading body")?;
    let (body_dto, attachment_dtos) = body_deser.0.split();
    let body = BodySource::try_from(body_dto).context("deserializing body")?;
    let attachments = attachment_dtos
        .into_iter()
        .map(catapulte_domain::entity::attachment::AttachmentRef::from)
        .collect();
    let recipients: sqlx::types::Json<Vec<RecipientDto>> =
        row.try_get("recipients").context("reading recipients")?;
    let variables: sqlx::types::Json<serde_json::Map<String, serde_json::Value>> =
        row.try_get("variables").context("reading variables")?;
    let idempotency_key = row
        .try_get("idempotency_key")
        .context("reading idempotency_key")?;
    let correlation_id: Option<String> = row
        .try_get("correlation_id")
        .context("reading correlation_id")?;
    let subject = row.try_get("subject").context("reading subject")?;
    let sender = row.try_get("sender").context("reading sender")?;
    Ok(Envelope {
        idempotency_key,
        correlation_id,
        subject,
        sender,
        recipients: recipients_from_dto(recipients.0),
        body,
        variables: variables.0,
        attachments,
    })
}

fn deserialize_trace_context(raw: Option<String>) -> TraceCarrier {
    let Some(json) = raw else {
        return TraceCarrier::default();
    };
    serde_json::from_str::<Vec<(String, String)>>(&json)
        .map(TraceCarrier::new)
        .unwrap_or_default()
}

impl PostgresAdapter {
    #[allow(clippy::too_many_lines)]
    async fn try_dequeue(&self) -> Result<Option<DequeuedEmail>, EmailQueueError> {
        let now = now_ms();

        let mut tx = self
            .pool()
            .begin()
            .await
            .context("beginning transaction")
            .map_err(|source| EmailQueueError::Storage { source })?;

        let maybe = sqlx::query(
            "SELECT id, email_id, attempt_count, trace_context FROM email_queue \
             WHERE claimed_until IS NULL OR claimed_until < $1 \
             ORDER BY enqueued_at ASC LIMIT 1 FOR UPDATE SKIP LOCKED",
        )
        .bind(now)
        .fetch_optional(&mut *tx)
        .await
        .context("finding next queue entry")
        .map_err(|source| EmailQueueError::Storage { source })?;

        let (entry_id, email_id_uuid, current_attempt, trace_raw): (
            uuid::Uuid,
            uuid::Uuid,
            u32,
            Option<String>,
        ) = match maybe {
            None => {
                tx.rollback()
                    .await
                    .context("rolling back empty dequeue")
                    .map_err(|source| EmailQueueError::Storage { source })?;
                return Ok(None);
            }
            Some(row) => {
                use sqlx::Row;
                let entry_id: uuid::Uuid = row
                    .try_get("id")
                    .context("reading entry id")
                    .map_err(|source| EmailQueueError::Storage { source })?;
                let email_id: uuid::Uuid = row
                    .try_get("email_id")
                    .context("reading email_id")
                    .map_err(|source| EmailQueueError::Storage { source })?;
                let attempt: i32 = row
                    .try_get("attempt_count")
                    .context("reading attempt_count")
                    .map_err(|source| EmailQueueError::Storage { source })?;
                let attempt_u32 = u32::try_from(attempt)
                    .context("attempt_count out of range")
                    .map_err(|source| EmailQueueError::Storage { source })?;
                let trace_raw: Option<String> = row
                    .try_get("trace_context")
                    .context("reading trace_context")
                    .map_err(|source| EmailQueueError::Storage { source })?;
                (entry_id, email_id, attempt_u32, trace_raw)
            }
        };

        let new_attempt = current_attempt + 1;
        let processing_timeout_ms: i64 = 300_000;
        let claim_until = now + processing_timeout_ms;

        sqlx::query(
            "UPDATE email_queue SET claimed_until = $1, attempt_count = attempt_count + 1 WHERE id = $2",
        )
        .bind(claim_until)
        .bind(entry_id)
        .execute(&mut *tx)
        .await
        .context("claiming queue entry")
        .map_err(|source| EmailQueueError::Storage { source })?;

        let maybe_row = sqlx::query(
            "SELECT idempotency_key, correlation_id, subject, sender, recipients, body, variables FROM emails WHERE id = $1",
        )
        .bind(email_id_uuid)
        .fetch_optional(&mut *tx)
        .await
        .context("fetching email for dequeue")
        .map_err(|source| EmailQueueError::Storage { source })?;

        match maybe_row {
            None => {
                sqlx::query("DELETE FROM email_queue WHERE id = $1")
                    .bind(entry_id)
                    .execute(&mut *tx)
                    .await
                    .context("deleting orphaned queue entry")
                    .map_err(|source| EmailQueueError::Storage { source })?;
                tx.commit()
                    .await
                    .context("committing orphan cleanup transaction")
                    .map_err(|source| EmailQueueError::Storage { source })?;
                Ok(None)
            }
            Some(row) => {
                tx.commit()
                    .await
                    .context("committing dequeue transaction")
                    .map_err(|source| EmailQueueError::Storage { source })?;
                let envelope =
                    parse_envelope(&row).map_err(|source| EmailQueueError::Storage { source })?;
                let token = AckToken::new(entry_id.as_bytes().to_vec());
                let trace = deserialize_trace_context(trace_raw);
                Ok(Some(DequeuedEmail {
                    id: EmailId::from(email_id_uuid),
                    envelope,
                    attempt: new_attempt,
                    token,
                    trace,
                }))
            }
        }
    }
}

impl EmailQueue for PostgresAdapter {
    async fn enqueue(&self, id: EmailId, _envelope: &Envelope) -> Result<(), EmailQueueError> {
        let entry_id = uuid::Uuid::now_v7();
        let email_id = id.as_uuid();
        let pairs = catapulte_telemetry::propagation::inject_current();
        let trace_context = if pairs.is_empty() {
            None
        } else {
            Some(
                serde_json::to_string(&pairs)
                    .context("serializing trace context")
                    .map_err(|source| EmailQueueError::Storage { source })?,
            )
        };
        sqlx::query("INSERT INTO email_queue (id, email_id, trace_context) VALUES ($1, $2, $3)")
            .bind(entry_id)
            .bind(email_id)
            .bind(trace_context)
            .execute(self.pool())
            .await
            .context("inserting into email_queue")
            .map_err(|source| EmailQueueError::Storage { source })?;
        Ok(())
    }

    async fn dequeue(&self) -> Result<DequeuedEmail, EmailQueueError> {
        loop {
            if let Some(item) = self.try_dequeue().await? {
                return Ok(item);
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }

    async fn ack(&self, token: AckToken) -> Result<(), EmailQueueError> {
        let entry_id = uuid::Uuid::from_slice(&token.0)
            .context("invalid ack token")
            .map_err(|source| EmailQueueError::Storage { source })?;
        sqlx::query("DELETE FROM email_queue WHERE id = $1")
            .bind(entry_id)
            .execute(self.pool())
            .await
            .context("deleting from email_queue")
            .map_err(|source| EmailQueueError::Storage { source })?;
        Ok(())
    }

    async fn nack(&self, token: AckToken, delay: Duration) -> Result<(), EmailQueueError> {
        let now_ms = now_ms();
        let delay_ms = i64::try_from(delay.as_millis()).unwrap_or(i64::MAX);
        let claimed_until = now_ms.saturating_add(delay_ms);
        let entry_id = uuid::Uuid::from_slice(&token.0)
            .context("invalid nack token")
            .map_err(|source| EmailQueueError::Storage { source })?;
        sqlx::query("UPDATE email_queue SET claimed_until = $1 WHERE id = $2")
            .bind(claimed_until)
            .bind(entry_id)
            .execute(self.pool())
            .await
            .context("nacking email_queue entry")
            .map_err(|source| EmailQueueError::Storage { source })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use catapulte_domain::entity::body::{BodySource, Plain};
    use catapulte_domain::entity::email::EmailId;
    use catapulte_domain::entity::envelope::Envelope;
    use catapulte_domain::port::email_queue::EmailQueue;
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
        Envelope {
            idempotency_key: None,
            correlation_id: None,
            subject: None,
            sender: "sender@example.com".to_owned(),
            recipients: vec![],
            body: BodySource::Plain(Plain::try_new(Some("hello".to_owned()), None).unwrap()),
            variables: serde_json::Map::new(),
            attachments: vec![],
        }
    }

    async fn save_and_enqueue(adapter: &PostgresAdapter, id: EmailId) {
        adapter.save(id, &sample_envelope()).await.unwrap();
        adapter.enqueue(id, &sample_envelope()).await.unwrap();
    }

    #[tokio::test]
    async fn enqueue_then_dequeue_returns_email() {
        let (adapter, _container) = fresh_adapter().await;
        let id = EmailId::default();
        save_and_enqueue(&adapter, id).await;

        let dequeued = adapter.dequeue().await.unwrap();
        assert_eq!(dequeued.id, id);
    }

    #[tokio::test]
    async fn ack_removes_item_from_queue() {
        let (adapter, _container) = fresh_adapter().await;
        let id = EmailId::default();
        save_and_enqueue(&adapter, id).await;

        let dequeued = adapter.dequeue().await.unwrap();
        assert_eq!(dequeued.id, id);

        adapter.ack(dequeued.token).await.unwrap();

        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM email_queue")
            .fetch_one(adapter.pool())
            .await
            .unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn try_dequeue_cleans_up_orphaned_entry_when_email_missing() {
        let (adapter, _container) = fresh_adapter().await;
        let id = EmailId::default();
        save_and_enqueue(&adapter, id).await;

        // Simulate an orphaned queue entry: delete the queue row first (so the FK
        // allows removing the email), then delete the email, then re-insert a raw
        // queue entry that references a now-missing email_id.
        let entry_id: uuid::Uuid = sqlx::query_scalar("SELECT id FROM email_queue")
            .fetch_one(adapter.pool())
            .await
            .unwrap();
        sqlx::query("DELETE FROM email_queue WHERE id = $1")
            .bind(entry_id)
            .execute(adapter.pool())
            .await
            .unwrap();
        sqlx::query("DELETE FROM emails WHERE id = $1")
            .bind(id.as_uuid())
            .execute(adapter.pool())
            .await
            .unwrap();
        // Re-insert the orphaned queue row on a single connection so the
        // session-level replication-role override and the INSERT share state.
        {
            let mut conn = adapter.pool().acquire().await.unwrap();
            sqlx::query("SET session_replication_role = replica")
                .execute(&mut *conn)
                .await
                .unwrap();
            sqlx::query("INSERT INTO email_queue (id, email_id) VALUES ($1, $2)")
                .bind(entry_id)
                .bind(id.as_uuid())
                .execute(&mut *conn)
                .await
                .unwrap();
            sqlx::query("SET session_replication_role = DEFAULT")
                .execute(&mut *conn)
                .await
                .unwrap();
        }

        // try_dequeue should return None and clean up the orphaned queue entry.
        let result = adapter.try_dequeue().await.unwrap();
        assert!(result.is_none());

        // The queue entry must be gone.
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM email_queue")
            .fetch_one(adapter.pool())
            .await
            .unwrap();
        assert_eq!(count, 0, "orphaned queue entry should have been deleted");
    }

    #[tokio::test]
    async fn nack_updates_claimed_until() {
        let (adapter, _container) = fresh_adapter().await;
        let id = EmailId::default();
        save_and_enqueue(&adapter, id).await;

        let dequeued = adapter.dequeue().await.unwrap();

        adapter
            .nack(dequeued.token, std::time::Duration::from_secs(10))
            .await
            .unwrap();

        // Item is still claimed; try_dequeue should return None immediately
        assert!(adapter.try_dequeue().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn null_trace_context_decodes_to_empty_carrier() {
        let (adapter, _container) = fresh_adapter().await;
        let id = EmailId::default();
        adapter.save(id, &sample_envelope()).await.unwrap();
        // Insert a queue row with NULL trace_context to simulate a pre-migration row.
        sqlx::query("INSERT INTO email_queue (id, email_id, trace_context) VALUES ($1, $2, NULL)")
            .bind(uuid::Uuid::now_v7())
            .bind(id.as_uuid())
            .execute(adapter.pool())
            .await
            .unwrap();

        let dequeued = adapter.dequeue().await.unwrap();
        assert!(
            dequeued.trace.is_empty(),
            "NULL trace_context must decode to an empty carrier"
        );
    }
}
