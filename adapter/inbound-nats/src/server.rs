use std::time::Duration;

use anyhow::Context;
use async_nats::HeaderMap;
use async_nats::jetstream;
use async_nats::jetstream::consumer::PullConsumer;
use catapulte_domain::use_case::submit_email::SubmitEmailUseCase;
use catapulte_inbound_http::dto::SubmitEmailRequest;
use futures_util::StreamExt;
use tokio_util::sync::CancellationToken;
use tracing::Instrument as _;

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

pub trait InboundNatsState: Clone + Send + Sync + 'static {
    fn submit_email(&self) -> &impl SubmitEmailUseCase;
}

#[derive(Debug)]
pub struct InboundNatsConfig {
    pub url: String,
    pub stream: String,
    pub subject: String,
    pub consumer: String,
    pub ack_wait_secs: u64,
    pub max_deliver: i64,
    pub backoff_secs: Vec<u64>,
}

impl InboundNatsConfig {
    /// # Errors
    ///
    /// Returns an error if the required env vars are unset or invalid.
    ///
    /// `<prefix>_URL` is the on/off switch; if unset or empty, returns `Ok(None)`
    /// so the operator can leave inbound NATS disabled.
    pub fn from_env(prefix: &str) -> anyhow::Result<Option<Self>> {
        Self::from_lookup(prefix, |key| std::env::var(key))
    }

    fn from_lookup<F>(prefix: &str, lookup: F) -> anyhow::Result<Option<Self>>
    where
        F: Fn(&str) -> Result<String, std::env::VarError>,
    {
        let Some(url) = lookup(&format!("{prefix}_URL"))
            .ok()
            .filter(|s| !s.is_empty())
        else {
            return Ok(None);
        };
        let stream = lookup(&format!("{prefix}_STREAM"))
            .with_context(|| format!("missing env var {prefix}_STREAM"))?;
        let subject = lookup(&format!("{prefix}_SUBJECT"))
            .with_context(|| format!("missing env var {prefix}_SUBJECT"))?;
        let consumer = lookup(&format!("{prefix}_CONSUMER"))
            .with_context(|| format!("missing env var {prefix}_CONSUMER"))?;
        let ack_wait_secs_key = format!("{prefix}_ACK_WAIT_SECS");
        let ack_wait_secs = match lookup(&ack_wait_secs_key) {
            Err(_) => 30u64,
            Ok(v) => {
                let parsed: u64 = v
                    .parse()
                    .with_context(|| format!("{ack_wait_secs_key} is not a valid u64: {v:?}"))?;
                anyhow::ensure!(
                    parsed > 0,
                    "{ack_wait_secs_key} must be greater than 0, got {parsed}"
                );
                parsed
            }
        };
        let max_deliver_key = format!("{prefix}_MAX_DELIVER");
        let max_deliver = match lookup(&max_deliver_key) {
            Err(_) => 5i64,
            Ok(v) => {
                let parsed: i64 = v
                    .parse()
                    .with_context(|| format!("{max_deliver_key} is not a valid integer: {v:?}"))?;
                anyhow::ensure!(
                    parsed > 0,
                    "{max_deliver_key} must be greater than 0, got {parsed}"
                );
                parsed
            }
        };
        let backoff_secs_key = format!("{prefix}_BACKOFF_SECS");
        let backoff_secs = match lookup(&backoff_secs_key) {
            Err(_) => vec![1u64, 5, 30],
            Ok(v) => {
                let entries: Result<Vec<u64>, _> =
                    v.split(',').map(|s| s.trim().parse::<u64>()).collect();
                let parsed = entries.with_context(|| {
                    format!("{backoff_secs_key} contains an unparseable entry: {v:?}")
                })?;
                anyhow::ensure!(!parsed.is_empty(), "{backoff_secs_key} must not be empty");
                parsed
            }
        };
        Ok(Some(Self {
            url,
            stream,
            subject,
            consumer,
            ack_wait_secs,
            max_deliver,
            backoff_secs,
        }))
    }

    /// # Errors
    ///
    /// Returns an error if NATS connection or consumer setup fails.
    pub async fn build(self) -> anyhow::Result<InboundNatsServer> {
        let client = async_nats::connect(&self.url)
            .await
            .context("connecting to NATS")?;
        let js = jetstream::new(client);

        let backoff: Vec<Duration> = self
            .backoff_secs
            .iter()
            .map(|&s| Duration::from_secs(s))
            .collect();

        let mut stream = js
            .get_or_create_stream(jetstream::stream::Config {
                name: self.stream.clone(),
                subjects: vec![self.subject.clone()],
                retention: jetstream::stream::RetentionPolicy::WorkQueue,
                storage: jetstream::stream::StorageType::File,
                ..Default::default()
            })
            .await
            .context("creating NATS stream")?;

        let stream_info = stream.info().await.context("fetching NATS stream info")?;
        anyhow::ensure!(
            stream_info.config.subjects.contains(&self.subject),
            "NATS stream {:?} does not include subject {:?}; found: {:?}",
            self.stream,
            self.subject,
            stream_info.config.subjects,
        );

        let consumer = stream
            .get_or_create_consumer(
                &self.consumer,
                jetstream::consumer::pull::Config {
                    durable_name: Some(self.consumer.clone()),
                    ack_policy: jetstream::consumer::AckPolicy::Explicit,
                    ack_wait: Duration::from_secs(self.ack_wait_secs),
                    max_deliver: self.max_deliver,
                    filter_subject: self.subject.clone(),
                    backoff,
                    ..Default::default()
                },
            )
            .await
            .context("creating NATS consumer")?;

        Ok(InboundNatsServer { consumer })
    }
}

pub struct InboundNatsServer {
    consumer: PullConsumer,
}

impl InboundNatsServer {
    pub async fn run<S: InboundNatsState>(self, state: S, cancel: CancellationToken) {
        loop {
            tokio::select! {
                biased;
                () = cancel.cancelled() => break,
                result = self.fetch_one() => match result {
                    Ok(Some(msg)) => self.handle_message(&state, msg).await,
                    Ok(None) => {}
                    Err(e) => {
                        tracing::error!(error = %e, "inbound NATS fetch failed");
                        tokio::time::sleep(Duration::from_secs(1)).await;
                    }
                }
            }
        }
        tracing::info!("inbound NATS server stopped");
    }

    async fn fetch_one(&self) -> anyhow::Result<Option<jetstream::Message>> {
        let mut batch = self
            .consumer
            .fetch()
            .max_messages(1)
            .expires(Duration::from_secs(5))
            .messages()
            .await
            .context("fetching from NATS consumer")?;

        match batch.next().await {
            None => Ok(None),
            Some(Ok(msg)) => Ok(Some(msg)),
            Some(Err(e)) => Err(nats_boxed_err(e).context("receiving NATS message")),
        }
    }

    async fn handle_message<S: InboundNatsState>(&self, state: &S, msg: jetstream::Message) {
        let trace_pairs = msg
            .headers
            .as_ref()
            .map(header_map_to_pairs)
            .unwrap_or_default();
        let span = tracing::info_span!("inbound_nats.submit");
        catapulte_telemetry::propagation::set_span_parent(&span, &trace_pairs);

        let request: SubmitEmailRequest = match serde_json::from_slice(&msg.payload) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "inbound NATS: unparseable payload, acking to discard");
                if let Err(ack_err) = msg.ack().await {
                    tracing::error!(error = %ack_err, "inbound NATS: failed to ack bad-payload message");
                }
                return;
            }
        };

        let input = match request.into_submit_input() {
            Ok(i) => i,
            Err(e) => {
                tracing::warn!(error = %e, "inbound NATS: invalid envelope, acking to discard");
                if let Err(ack_err) = msg.ack().await {
                    tracing::error!(error = %ack_err, "inbound NATS: failed to ack invalid-envelope message");
                }
                return;
            }
        };

        match state.submit_email().execute(input).instrument(span).await {
            Ok(id) => {
                tracing::debug!(email_id = %id.as_uuid(), "inbound NATS: email submitted");
                if let Err(e) = msg.ack().await {
                    tracing::error!(error = %e, "inbound NATS: failed to ack after submit");
                }
            }
            Err(e) if e.is_transient() => {
                tracing::warn!(error = %e, "inbound NATS: submit failed, nacking for retry");
                if let Err(nack_err) = msg.ack_with(jetstream::AckKind::Nak(None)).await {
                    tracing::error!(error = %nack_err, "inbound NATS: failed to nack after transient submit error");
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "inbound NATS: permanent submit failure, acking to discard");
                if let Err(ack_err) = msg.ack().await {
                    tracing::error!(error = %ack_err, "inbound NATS: failed to ack after permanent submit error");
                }
            }
        }
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
mod tests {
    use std::collections::HashMap;
    use std::env::VarError;

    use super::InboundNatsConfig;

    fn make_lookup(
        vars: HashMap<&'static str, &'static str>,
    ) -> impl Fn(&str) -> Result<String, VarError> {
        move |key: &str| {
            vars.get(key)
                .map(|v| (*v).to_owned())
                .ok_or(VarError::NotPresent)
        }
    }

    #[test]
    fn from_env_returns_none_when_url_unset() {
        let result = InboundNatsConfig::from_lookup("TEST", make_lookup(HashMap::new()));
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn from_env_returns_none_when_url_empty() {
        let mut vars = HashMap::new();
        vars.insert("TEST_URL", "");
        let result = InboundNatsConfig::from_lookup("TEST", make_lookup(vars));
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn from_env_returns_some_when_all_required_vars_set() {
        let mut vars = HashMap::new();
        vars.insert("TEST_URL", "nats://localhost:4222");
        vars.insert("TEST_STREAM", "MY_STREAM");
        vars.insert("TEST_SUBJECT", "my.subject");
        vars.insert("TEST_CONSUMER", "my-consumer");
        let result = InboundNatsConfig::from_lookup("TEST", make_lookup(vars));
        let cfg = result.expect("should succeed").expect("should be Some");
        assert_eq!(cfg.url, "nats://localhost:4222");
        assert_eq!(cfg.stream, "MY_STREAM");
        assert_eq!(cfg.subject, "my.subject");
        assert_eq!(cfg.consumer, "my-consumer");
        assert_eq!(cfg.ack_wait_secs, 30);
        assert_eq!(cfg.max_deliver, 5);
        assert_eq!(cfg.backoff_secs, vec![1, 5, 30]);
    }

    #[test]
    fn from_env_returns_some_with_custom_optional_vars() {
        let mut vars = HashMap::new();
        vars.insert("TEST_URL", "nats://localhost:4222");
        vars.insert("TEST_STREAM", "S");
        vars.insert("TEST_SUBJECT", "s");
        vars.insert("TEST_CONSUMER", "c");
        vars.insert("TEST_ACK_WAIT_SECS", "60");
        vars.insert("TEST_MAX_DELIVER", "10");
        vars.insert("TEST_BACKOFF_SECS", "2, 10, 60");
        let result = InboundNatsConfig::from_lookup("TEST", make_lookup(vars));
        let cfg = result.expect("should succeed").expect("should be Some");
        assert_eq!(cfg.ack_wait_secs, 60);
        assert_eq!(cfg.max_deliver, 10);
        assert_eq!(cfg.backoff_secs, vec![2, 10, 60]);
    }

    #[test]
    fn from_env_errors_when_url_set_but_stream_missing() {
        let mut vars = HashMap::new();
        vars.insert("TEST_URL", "nats://localhost:4222");
        let result = InboundNatsConfig::from_lookup("TEST", make_lookup(vars));
        let err = result.expect_err("should fail when STREAM is missing");
        assert!(
            format!("{err}").contains("TEST_STREAM"),
            "error should mention the missing var, got: {err}"
        );
    }

    #[test]
    fn from_env_errors_when_url_set_but_subject_missing() {
        let mut vars = HashMap::new();
        vars.insert("TEST_URL", "nats://localhost:4222");
        vars.insert("TEST_STREAM", "S");
        let result = InboundNatsConfig::from_lookup("TEST", make_lookup(vars));
        let err = result.expect_err("should fail when SUBJECT is missing");
        assert!(
            format!("{err}").contains("TEST_SUBJECT"),
            "error should mention the missing var, got: {err}"
        );
    }

    #[test]
    fn from_env_errors_when_url_set_but_consumer_missing() {
        let mut vars = HashMap::new();
        vars.insert("TEST_URL", "nats://localhost:4222");
        vars.insert("TEST_STREAM", "S");
        vars.insert("TEST_SUBJECT", "s");
        let result = InboundNatsConfig::from_lookup("TEST", make_lookup(vars));
        let err = result.expect_err("should fail when CONSUMER is missing");
        assert!(
            format!("{err}").contains("TEST_CONSUMER"),
            "error should mention the missing var, got: {err}"
        );
    }

    fn base_vars() -> HashMap<&'static str, &'static str> {
        let mut vars = HashMap::new();
        vars.insert("TEST_URL", "nats://localhost:4222");
        vars.insert("TEST_STREAM", "S");
        vars.insert("TEST_SUBJECT", "s");
        vars.insert("TEST_CONSUMER", "c");
        vars
    }

    #[test]
    fn from_env_errors_when_ack_wait_secs_is_not_a_number() {
        let mut vars = base_vars();
        vars.insert("TEST_ACK_WAIT_SECS", "not-a-number");
        let result = InboundNatsConfig::from_lookup("TEST", make_lookup(vars));
        let err = result.expect_err("should fail when ACK_WAIT_SECS is not a number");
        assert!(
            format!("{err}").contains("TEST_ACK_WAIT_SECS"),
            "error should mention the var name, got: {err}"
        );
    }

    #[test]
    fn from_env_errors_when_ack_wait_secs_is_zero() {
        let mut vars = base_vars();
        vars.insert("TEST_ACK_WAIT_SECS", "0");
        let result = InboundNatsConfig::from_lookup("TEST", make_lookup(vars));
        let err = result.expect_err("should fail when ACK_WAIT_SECS is zero");
        assert!(
            format!("{err}").contains("TEST_ACK_WAIT_SECS"),
            "error should mention the var name, got: {err}"
        );
    }

    #[test]
    fn from_env_errors_when_max_deliver_is_not_a_number() {
        let mut vars = base_vars();
        vars.insert("TEST_MAX_DELIVER", "bad");
        let result = InboundNatsConfig::from_lookup("TEST", make_lookup(vars));
        let err = result.expect_err("should fail when MAX_DELIVER is not a number");
        assert!(
            format!("{err}").contains("TEST_MAX_DELIVER"),
            "error should mention the var name, got: {err}"
        );
    }

    #[test]
    fn from_env_errors_when_max_deliver_is_zero() {
        let mut vars = base_vars();
        vars.insert("TEST_MAX_DELIVER", "0");
        let result = InboundNatsConfig::from_lookup("TEST", make_lookup(vars));
        let err = result.expect_err("should fail when MAX_DELIVER is zero");
        assert!(
            format!("{err}").contains("TEST_MAX_DELIVER"),
            "error should mention the var name, got: {err}"
        );
    }

    #[test]
    fn from_env_errors_when_backoff_secs_has_unparseable_entry() {
        let mut vars = base_vars();
        vars.insert("TEST_BACKOFF_SECS", "1,bad,30");
        let result = InboundNatsConfig::from_lookup("TEST", make_lookup(vars));
        let err = result.expect_err("should fail when BACKOFF_SECS has an unparseable entry");
        assert!(
            format!("{err}").contains("TEST_BACKOFF_SECS"),
            "error should mention the var name, got: {err}"
        );
    }

    #[test]
    fn from_env_errors_when_backoff_secs_is_empty_after_trimming() {
        let mut vars = base_vars();
        vars.insert("TEST_BACKOFF_SECS", "  ,  ,  ");
        let result = InboundNatsConfig::from_lookup("TEST", make_lookup(vars));
        let err = result.expect_err("should fail when BACKOFF_SECS trims to empty");
        assert!(
            format!("{err}").contains("TEST_BACKOFF_SECS"),
            "error should mention the var name, got: {err}"
        );
    }
}
