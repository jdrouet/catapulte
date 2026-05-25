pub mod dto;
pub mod email_queue;
pub mod event_publisher;

use anyhow::Context;
use async_nats::jetstream;

#[derive(Clone)]
pub struct NatsAdapter {
    inner: std::sync::Arc<NatsAdapterInner>,
}

struct NatsAdapterInner {
    client: async_nats::Client,
    consumer: jetstream::consumer::PullConsumer,
    subject: String,
}

impl NatsAdapter {
    /// # Errors
    ///
    /// Returns an error if the connection, stream, or consumer setup fails.
    pub async fn connect(config: &NatsConfig) -> anyhow::Result<Self> {
        let client = async_nats::connect(&config.url)
            .await
            .context("connecting to NATS")?;
        let js = jetstream::new(client.clone());

        let backoff: Vec<std::time::Duration> = config
            .backoff_secs
            .iter()
            .map(|&s| std::time::Duration::from_secs(s))
            .collect();

        let mut stream = js
            .get_or_create_stream(jetstream::stream::Config {
                name: config.stream.clone(),
                subjects: vec![config.subject.clone()],
                retention: jetstream::stream::RetentionPolicy::WorkQueue,
                storage: jetstream::stream::StorageType::File,
                ..Default::default()
            })
            .await
            .context("creating NATS stream")?;

        let stream_info = stream.info().await.context("fetching NATS stream info")?;
        anyhow::ensure!(
            stream_info.config.subjects.contains(&config.subject),
            "NATS stream {:?} does not include subject {:?}; found: {:?}",
            config.stream,
            config.subject,
            stream_info.config.subjects,
        );

        let consumer = stream
            .get_or_create_consumer(
                &config.consumer,
                jetstream::consumer::pull::Config {
                    durable_name: Some(config.consumer.clone()),
                    ack_policy: jetstream::consumer::AckPolicy::Explicit,
                    ack_wait: std::time::Duration::from_secs(config.ack_wait_secs),
                    max_deliver: config.max_deliver,
                    filter_subject: config.subject.clone(),
                    backoff,
                    ..Default::default()
                },
            )
            .await
            .context("creating NATS consumer")?;

        Ok(Self {
            inner: std::sync::Arc::new(NatsAdapterInner {
                client,
                consumer,
                subject: config.subject.clone(),
            }),
        })
    }

    pub(crate) fn client(&self) -> &async_nats::Client {
        &self.inner.client
    }

    pub(crate) fn consumer(&self) -> &jetstream::consumer::PullConsumer {
        &self.inner.consumer
    }

    pub(crate) fn subject(&self) -> &str {
        &self.inner.subject
    }
}

pub struct NatsConfig {
    pub url: String,
    pub stream: String,
    pub subject: String,
    pub consumer: String,
    pub ack_wait_secs: u64,
    pub max_deliver: i64,
    pub backoff_secs: Vec<u64>,
}

impl NatsConfig {
    /// # Errors
    ///
    /// Returns an error if a required env var is missing or unparseable.
    pub fn from_env(prefix: &str) -> anyhow::Result<Self> {
        let url_key = format!("{prefix}_URL");
        let url = std::env::var(&url_key).with_context(|| format!("missing env var {url_key}"))?;

        let stream = std::env::var(format!("{prefix}_STREAM"))
            .unwrap_or_else(|_| "CATAPULTE_EMAILS".to_owned());
        let subject = std::env::var(format!("{prefix}_SUBJECT"))
            .unwrap_or_else(|_| "catapulte.emails.queued".to_owned());
        let consumer = std::env::var(format!("{prefix}_CONSUMER"))
            .unwrap_or_else(|_| "catapulte-worker".to_owned());
        let ack_wait_secs = std::env::var(format!("{prefix}_ACK_WAIT_SECS"))
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(30u64);
        let max_deliver = std::env::var(format!("{prefix}_MAX_DELIVER"))
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(3i64);
        let backoff_secs = std::env::var(format!("{prefix}_BACKOFF")).ok().map_or_else(
            || vec![30, 60, 120],
            |v| {
                v.split(',')
                    .filter_map(|s| s.trim().parse::<u64>().ok())
                    .collect()
            },
        );

        Ok(Self {
            url,
            stream,
            subject,
            consumer,
            ack_wait_secs,
            max_deliver,
            backoff_secs,
        })
    }

    /// # Errors
    ///
    /// Returns an error if the adapter fails to connect or set up the stream/consumer.
    pub async fn build(self) -> anyhow::Result<NatsAdapter> {
        NatsAdapter::connect(&self).await
    }
}
