use std::time::Duration;

use anyhow::Context;
use async_nats::HeaderMap;
use catapulte_domain::entity::email::EmailId;
use catapulte_domain::entity::envelope::Envelope;
use catapulte_domain::port::email_queue::{
    AckToken, DequeuedEmail, EmailQueue, EmailQueueError, TraceCarrier,
};
use futures_util::StreamExt;

use crate::NatsAdapter;
use crate::dto::QueuedEmailPayload;

/// Convert W3C trace-context pairs into a NATS `HeaderMap`.
/// Returns `None` when the carrier is empty so callers can skip header publish.
fn pairs_to_header_map(pairs: &[(String, String)]) -> Option<HeaderMap> {
    if pairs.is_empty() {
        return None;
    }
    let mut map = HeaderMap::new();
    for (k, v) in pairs {
        map.insert(k.as_str(), v.as_str());
    }
    Some(map)
}

/// Extract only the W3C trace-context headers from a NATS `HeaderMap`.
///
/// Keeps at most one `traceparent` pair (the first value) and at most one
/// `tracestate` pair (all values comma-joined in order, as required by the W3C
/// spec). Every other header is dropped to prevent arbitrary or PII headers from
/// leaking into the trace carrier.
///
/// Header-name matching is case-insensitive so that external producers using
/// `Traceparent` or `TRACEPARENT` are handled correctly.
fn header_map_to_pairs(headers: &HeaderMap) -> Vec<(String, String)> {
    let mut pairs: Vec<(String, String)> = Vec::with_capacity(2);

    // traceparent — single-valued by spec; take the first value present.
    let traceparent: Option<String> = headers
        .iter()
        .find(|(name, _)| name.to_string().eq_ignore_ascii_case("traceparent"))
        .and_then(|(_, values)| values.first().map(|v| v.as_str().to_owned()));
    if let Some(v) = traceparent {
        pairs.push(("traceparent".to_owned(), v));
    }

    // tracestate — ordered comma-separated list; join all values in order.
    let tracestate_values: Vec<String> = headers
        .iter()
        .find(|(name, _)| name.to_string().eq_ignore_ascii_case("tracestate"))
        .map(|(_, values)| values.iter().map(|v| v.as_str().to_owned()).collect())
        .unwrap_or_default();
    if !tracestate_values.is_empty() {
        pairs.push(("tracestate".to_owned(), tracestate_values.join(",")));
    }

    pairs
}

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
        let trace_pairs = catapulte_telemetry::propagation::inject_current();
        if let Some(headers) = pairs_to_header_map(&trace_pairs) {
            self.client()
                .publish_with_headers(self.subject().to_owned(), headers, bytes.into())
                .await
                .map_err(nats_err)
                .context("publishing to NATS")
                .map_err(|source| EmailQueueError::Storage { source })?;
        } else {
            self.client()
                .publish(self.subject().to_owned(), bytes.into())
                .await
                .map_err(nats_err)
                .context("publishing to NATS")
                .map_err(|source| EmailQueueError::Storage { source })?;
        }
        Ok(())
    }

    async fn dequeue(&self) -> Result<DequeuedEmail, EmailQueueError> {
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

                let trace_pairs = msg
                    .headers
                    .as_ref()
                    .map(header_map_to_pairs)
                    .unwrap_or_default();

                return Ok(DequeuedEmail {
                    id: email_id,
                    envelope,
                    attempt,
                    token,
                    trace: TraceCarrier::new(trace_pairs),
                });
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
mod header_filter_tests {
    use async_nats::HeaderMap;

    use super::header_map_to_pairs;

    #[test]
    fn non_w3c_headers_are_dropped() {
        let mut map = HeaderMap::new();
        map.insert("x-custom", "should-be-dropped");
        map.insert("authorization", "Bearer secret");
        let pairs = header_map_to_pairs(&map);
        assert!(
            pairs.is_empty(),
            "non-W3C headers must be dropped; got: {pairs:?}"
        );
    }

    #[test]
    fn traceparent_is_preserved() {
        let mut map = HeaderMap::new();
        map.insert(
            "traceparent",
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
        );
        map.insert("x-custom", "noise");
        let pairs = header_map_to_pairs(&map);
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].0, "traceparent");
        assert_eq!(
            pairs[0].1,
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"
        );
    }

    #[test]
    fn multiple_tracestate_values_are_comma_joined_in_order() {
        let mut map = HeaderMap::new();
        map.append("tracestate", "vendor1=value1");
        map.append("tracestate", "vendor2=value2");
        map.append("tracestate", "vendor3=value3");
        let pairs = header_map_to_pairs(&map);
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].0, "tracestate");
        assert_eq!(pairs[0].1, "vendor1=value1,vendor2=value2,vendor3=value3");
    }

    #[test]
    fn traceparent_and_tracestate_together_with_noise() {
        let mut map = HeaderMap::new();
        map.insert(
            "traceparent",
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
        );
        map.append("tracestate", "rojo=00f067aa0ba902b7");
        map.append("tracestate", "congo=t61rcWkgMzE");
        map.insert("authorization", "Bearer leaked-secret");
        let pairs = header_map_to_pairs(&map);
        // exactly two pairs, no authorization
        assert_eq!(pairs.len(), 2);
        let has_auth = pairs
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("authorization"));
        assert!(!has_auth, "authorization must not appear in pairs");
    }
}

#[cfg(test)]
mod nak_tests {
    use super::nak_payload;
    use std::time::Duration;

    #[test]
    fn one_hour_delay_formats_correctly() {
        let s = nak_payload(Duration::from_hours(1));
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

    async fn wait_for_tcp(port: u16, timeout: Duration) {
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
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    async fn fresh_adapter() -> (NatsAdapter, testcontainers::ContainerAsync<GenericImage>) {
        let nats = GenericImage::new("nats", "2-alpine")
            .with_wait_for(WaitFor::message_on_stderr("Server is ready"))
            .with_cmd(["--js".to_owned()])
            .start()
            .await
            .expect("failed to start NATS container; ensure Docker is running");

        let port = nats.get_host_port_ipv4(4222).await.unwrap();
        wait_for_tcp(port, Duration::from_secs(15)).await;
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
            correlation_id: None,
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

        let dequeued = tokio::time::timeout(Duration::from_secs(10), adapter.dequeue())
            .await
            .expect("dequeue timed out")
            .unwrap();

        assert_eq!(dequeued.id, id);
        assert_eq!(dequeued.envelope.sender, envelope.sender);
        assert_eq!(dequeued.envelope.subject, envelope.subject);
    }

    #[serial_test::serial]
    #[tokio::test]
    async fn dequeue_returns_attempt_one_on_first_delivery() {
        let (adapter, _nats) = fresh_adapter().await;
        let id = EmailId::default();
        let envelope = sample_envelope();

        adapter.enqueue(id, &envelope).await.unwrap();

        let dequeued = tokio::time::timeout(Duration::from_secs(10), adapter.dequeue())
            .await
            .expect("dequeue timed out")
            .unwrap();

        assert_eq!(dequeued.attempt, 1);
    }

    #[serial_test::serial]
    #[tokio::test]
    async fn ack_removes_message() {
        let (adapter, _nats) = fresh_adapter().await;
        let id = EmailId::default();
        let envelope = sample_envelope();

        adapter.enqueue(id, &envelope).await.unwrap();

        let dequeued = tokio::time::timeout(Duration::from_secs(10), adapter.dequeue())
            .await
            .expect("dequeue timed out")
            .unwrap();

        adapter.ack(dequeued.token).await.unwrap();

        let result = tokio::time::timeout(Duration::from_secs(8), adapter.dequeue()).await;
        assert!(result.is_err(), "expected dequeue to time out after ack");
    }
}
