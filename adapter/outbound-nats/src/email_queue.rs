use std::time::Duration;

use anyhow::Context;
use catapulte_domain::entity::email::EmailId;
use catapulte_domain::entity::envelope::Envelope;
use catapulte_domain::port::email_queue::{AckToken, EmailQueue, EmailQueueError};
use futures_util::StreamExt;

use crate::NatsAdapter;
use crate::dto::QueuedEmailPayload;

fn nats_err<E>(e: E) -> anyhow::Error
where
    E: std::error::Error + Send + Sync + 'static,
{
    anyhow::Error::from(e)
}

/// Wraps a boxed `async-nats` error into [`anyhow::Error`] without
/// stringifying the underlying cause.
struct NatsBoxedError(async_nats::Error);

impl std::fmt::Debug for NatsBoxedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.0)
    }
}

impl std::fmt::Display for NatsBoxedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for NatsBoxedError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.0.as_ref())
    }
}

fn nats_boxed_err(e: async_nats::Error) -> anyhow::Error {
    anyhow::Error::from(NatsBoxedError(e))
}

fn nak_payload(delay: std::time::Duration) -> String {
    let delay_ns = i64::try_from(delay.as_nanos()).unwrap_or(i64::MAX);
    format!("-NAK {{\"delay\":{delay_ns}}}")
}

impl EmailQueue for NatsAdapter {
    async fn enqueue(&self, id: EmailId, envelope: &Envelope) -> Result<(), EmailQueueError> {
        let payload = QueuedEmailPayload::from((&id, envelope));
        let bytes = serde_json::to_vec(&payload)
            .context("serializing envelope")
            .map_err(|source| EmailQueueError::Storage { source })?;
        self.client()
            .publish(self.subject().to_owned(), bytes.into())
            .await
            .map_err(nats_err)
            .context("publishing to NATS")
            .map_err(|source| EmailQueueError::Storage { source })?;
        Ok(())
    }

    async fn dequeue(&self) -> Result<(EmailId, Envelope, u32, AckToken), EmailQueueError> {
        loop {
            let mut batch = self
                .consumer()
                .fetch()
                .max_messages(1)
                .expires(Duration::from_secs(5))
                .messages()
                .await
                .map_err(nats_err)
                .context("fetching from NATS consumer")
                .map_err(|source| EmailQueueError::Storage { source })?;

            if let Some(result) = batch.next().await {
                let msg = result
                    .map_err(nats_boxed_err)
                    .context("receiving NATS message")
                    .map_err(|source| EmailQueueError::Storage { source })?;

                let info = msg
                    .info()
                    .map_err(nats_boxed_err)
                    .context("reading message info")
                    .map_err(|source| EmailQueueError::Storage { source })?;

                let attempt = u32::try_from(info.delivered).unwrap_or(1).max(1);

                let reply = msg
                    .reply
                    .as_ref()
                    .context("message has no reply subject")
                    .map_err(|source| EmailQueueError::Storage { source })?
                    .as_str()
                    .as_bytes()
                    .to_vec();
                let token = AckToken::new(reply);

                let payload: QueuedEmailPayload = serde_json::from_slice(&msg.payload)
                    .context("deserializing NATS payload")
                    .map_err(|source| EmailQueueError::Storage { source })?;

                let (email_id, envelope) = <(EmailId, Envelope)>::try_from(payload)
                    .map_err(|source| EmailQueueError::Storage { source })?;

                return Ok((email_id, envelope, attempt, token));
            }
        }
    }

    async fn ack(&self, token: AckToken) -> Result<(), EmailQueueError> {
        let reply = String::from_utf8(token.into_bytes())
            .context("invalid ack token encoding")
            .map_err(|source| EmailQueueError::Storage { source })?;
        self.client()
            .publish(reply, b"+ACK"[..].into())
            .await
            .map_err(nats_err)
            .context("publishing ack to NATS")
            .map_err(|source| EmailQueueError::Storage { source })?;
        Ok(())
    }

    async fn nack(&self, token: AckToken, delay: Duration) -> Result<(), EmailQueueError> {
        let reply = String::from_utf8(token.into_bytes())
            .context("invalid nack token encoding")
            .map_err(|source| EmailQueueError::Storage { source })?;
        let payload = nak_payload(delay);
        self.client()
            .publish(reply, payload.into())
            .await
            .map_err(nats_err)
            .context("publishing nack to NATS")
            .map_err(|source| EmailQueueError::Storage { source })?;
        Ok(())
    }
}

#[cfg(test)]
mod nak_tests {
    use super::nak_payload;
    use std::time::Duration;

    #[test]
    fn one_hour_delay_formats_correctly() {
        let s = nak_payload(Duration::from_secs(3600));
        assert_eq!(s, format!("-NAK {{\"delay\":{}}}", 3_600_000_000_000i64));
    }

    #[test]
    fn overflow_delay_clamps_to_i64_max() {
        // Duration::from_nanos(u64::MAX) has u64::MAX nanos > i64::MAX nanos.
        let huge = Duration::from_nanos(u64::MAX);
        let s = nak_payload(huge);
        assert_eq!(s, format!("-NAK {{\"delay\":{}}}", i64::MAX));
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use catapulte_domain::entity::body::{BodySource, Plain};
    use catapulte_domain::entity::email::EmailId;
    use catapulte_domain::entity::envelope::Envelope;
    use catapulte_domain::port::email_queue::EmailQueue;
    use testcontainers::GenericImage;
    use testcontainers::ImageExt;
    use testcontainers::core::WaitFor;
    use testcontainers::runners::AsyncRunner;

    use crate::{NatsAdapter, NatsConfig};

    async fn fresh_adapter() -> (NatsAdapter, testcontainers::ContainerAsync<GenericImage>) {
        let nats = GenericImage::new("nats", "2-alpine")
            .with_wait_for(WaitFor::message_on_stderr("Server is ready"))
            .with_cmd(["--js".to_owned()])
            .start()
            .await
            .expect("failed to start NATS container; ensure Docker is running");

        let port = nats.get_host_port_ipv4(4222).await.unwrap();
        let adapter = NatsConfig {
            url: format!("nats://127.0.0.1:{port}"),
            stream: "TEST".to_owned(),
            subject: "test.queued".to_owned(),
            consumer: "worker".to_owned(),
            ack_wait_secs: 3,
            max_deliver: 3,
            backoff_secs: vec![1, 2, 3],
        }
        .build()
        .await
        .expect("failed to build NATS adapter");
        (adapter, nats)
    }

    fn sample_envelope() -> Envelope {
        Envelope {
            idempotency_key: None,
            subject: Some("Test subject".to_owned()),
            sender: "sender@example.com".to_owned(),
            recipients: vec![],
            body: BodySource::Plain(Plain::try_new(Some("hello".to_owned()), None).unwrap()),
            variables: serde_json::Map::new(),
            attachments: vec![],
        }
    }

    #[serial_test::serial]
    #[tokio::test]
    async fn enqueue_then_dequeue_returns_same_id_and_envelope() {
        let (adapter, _nats) = fresh_adapter().await;
        let id = EmailId::default();
        let envelope = sample_envelope();

        adapter.enqueue(id, &envelope).await.unwrap();

        let (returned_id, returned_envelope, _, _token) =
            tokio::time::timeout(Duration::from_secs(10), adapter.dequeue())
                .await
                .expect("dequeue timed out")
                .unwrap();

        assert_eq!(returned_id, id);
        assert_eq!(returned_envelope.sender, envelope.sender);
        assert_eq!(returned_envelope.subject, envelope.subject);
    }

    #[serial_test::serial]
    #[tokio::test]
    async fn dequeue_returns_attempt_one_on_first_delivery() {
        let (adapter, _nats) = fresh_adapter().await;
        let id = EmailId::default();
        let envelope = sample_envelope();

        adapter.enqueue(id, &envelope).await.unwrap();

        let (_, _, attempt, _token) =
            tokio::time::timeout(Duration::from_secs(10), adapter.dequeue())
                .await
                .expect("dequeue timed out")
                .unwrap();

        assert_eq!(attempt, 1);
    }

    #[serial_test::serial]
    #[tokio::test]
    async fn ack_removes_message() {
        let (adapter, _nats) = fresh_adapter().await;
        let id = EmailId::default();
        let envelope = sample_envelope();

        adapter.enqueue(id, &envelope).await.unwrap();

        let (_, _, _, token) = tokio::time::timeout(Duration::from_secs(10), adapter.dequeue())
            .await
            .expect("dequeue timed out")
            .unwrap();

        adapter.ack(token).await.unwrap();

        let result = tokio::time::timeout(Duration::from_secs(8), adapter.dequeue()).await;
        assert!(result.is_err(), "expected dequeue to time out after ack");
    }
}
