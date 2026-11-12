pub mod dto;
pub mod email_queue;
pub mod event_publisher;
mod health;

use anyhow::Context;
use async_nats::jetstream;

#[derive(Clone)]
pub struct NatsAdapter {
    inner: std::sync::Arc<NatsAdapterInner>,
}

struct NatsAdapterInner {
    client: async_nats::Client,
    jetstream: jetstream::Context,
    consumer: jetstream::consumer::PullConsumer,
    subject: String,
    renew_interval: std::time::Duration,
    supports_scheduling: bool,
    schedule_limit: Option<std::time::Duration>,
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

        let stream_config = stream
            .info()
            .await
            .context("fetching NATS stream info")?
            .config
            .clone();
        anyhow::ensure!(
            stream_config.subjects.contains(&config.subject),
            "NATS stream {:?} does not include subject {:?}; found: {:?}",
            config.stream,
            config.subject,
            stream_config.subjects,
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

        // An existing durable keeps its own settings, so read them back.
        let observed = &consumer.cached_info().config;
        let deadline = effective_deadline(observed.ack_wait, &observed.backoff);
        let others = match other_consumers(&stream, &config.consumer).await {
            Ok(others) => Some(others),
            Err(e) => {
                tracing::warn!(error = %e, "could not list the consumers of the NATS stream");
                None
            }
        };
        let blocker = match &others {
            Some(others) => scheduling_blocker(
                &stream_config,
                observed,
                others,
                &config.subject,
                &client.server_info().version,
                deadline,
            ),
            None => Some("the stream's other consumers could not be listed"),
        };
        let supports_scheduling = match blocker {
            Some(reason) => {
                tracing::warn!(
                    reason,
                    "scheduled sends are not available on this NATS queue"
                );
                false
            }
            None => enable_schedules(&js, &stream_config, &config.subject).await,
        };

        Ok(Self {
            inner: std::sync::Arc::new(NatsAdapterInner {
                client,
                jetstream: js,
                consumer,
                subject: config.subject.clone(),
                renew_interval: deadline / 3,
                supports_scheduling,
                schedule_limit: schedule_limit(stream_config.max_age),
            }),
        })
    }

    /// Returns the number of messages pending in the NATS consumer
    /// (i.e. messages not yet delivered).
    ///
    /// # Errors
    ///
    /// Returns an error if the consumer info request fails.
    pub async fn pending(&self) -> anyhow::Result<u64> {
        use anyhow::Context as _;
        let mut consumer = self.inner.consumer.clone();
        let info = consumer
            .info()
            .await
            .context("fetching NATS consumer info")?;
        Ok(info.num_pending)
    }

    pub(crate) fn client(&self) -> &async_nats::Client {
        &self.inner.client
    }

    pub(crate) fn jetstream(&self) -> &jetstream::Context {
        &self.inner.jetstream
    }

    pub(crate) fn consumer(&self) -> &jetstream::consumer::PullConsumer {
        &self.inner.consumer
    }

    pub(crate) fn subject(&self) -> &str {
        &self.inner.subject
    }

    /// True when a future `send_at_ms` can be held by `JetStream` message schedules.
    #[must_use]
    pub fn supports_scheduling(&self) -> bool {
        self.inner.supports_scheduling
    }

    /// How far ahead a schedule may be set before the stream's `max_age` could expire it;
    /// `None` when the stream keeps messages forever.
    #[must_use]
    pub fn schedule_limit(&self) -> Option<std::time::Duration> {
        self.inner.schedule_limit
    }

    pub(crate) fn renew_interval(&self) -> std::time::Duration {
        self.inner.renew_interval
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
        let ack_wait_key = format!("{prefix}_ACK_WAIT_SECS");
        let ack_wait_secs = match std::env::var(&ack_wait_key) {
            Err(_) => 30u64,
            Ok(v) => v
                .parse::<u64>()
                .with_context(|| format!("invalid {ack_wait_key}: {v:?}"))?,
        };

        let max_deliver_key = format!("{prefix}_MAX_DELIVER");
        let max_deliver = match std::env::var(&max_deliver_key) {
            Err(_) => 3i64,
            Ok(v) => v
                .parse::<i64>()
                .with_context(|| format!("invalid {max_deliver_key}: {v:?}"))?,
        };

        let backoff_key = format!("{prefix}_BACKOFF");
        let backoff_secs = match std::env::var(&backoff_key) {
            Err(_) => vec![30u64, 60, 120],
            Ok(v) => v
                .split(',')
                .map(|s| {
                    s.trim()
                        .parse::<u64>()
                        .with_context(|| format!("invalid value in {backoff_key}: {s:?}"))
                })
                .collect::<anyhow::Result<Vec<_>>>()?,
        };

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
        Box::pin(NatsAdapter::connect(&self)).await
    }
}

/// The smallest of `ack_wait` and every backoff entry, where an `ack_wait` of zero means the
/// server default of 30 s.
fn effective_deadline(
    ack_wait: std::time::Duration,
    backoff: &[std::time::Duration],
) -> std::time::Duration {
    let ack_wait = if ack_wait.is_zero() {
        std::time::Duration::from_secs(30)
    } else {
        ack_wait
    };
    backoff
        .iter()
        .copied()
        .fold(ack_wait, std::time::Duration::min)
}

/// Covers the up-to-one-second `@at` rounding and clock skew between the instance and the server.
const SCHEDULE_MARGIN: std::time::Duration = std::time::Duration::from_mins(5);

fn schedule_limit(max_age: std::time::Duration) -> Option<std::time::Duration> {
    (!max_age.is_zero()).then(|| max_age.saturating_sub(SCHEDULE_MARGIN))
}

/// Whether two subjects or subject patterns (with `*` and `>`) can match a common subject.
fn subjects_overlap(a: &str, b: &str) -> bool {
    let (mut a, mut b) = (a.split('.'), b.split('.'));
    loop {
        match (a.next(), b.next()) {
            (None, None) | (Some(">"), Some(_)) | (Some(_), Some(">")) => return true,
            (Some(x), Some(y)) if x == y || x == "*" || y == "*" => {}
            _ => return false,
        }
    }
}

/// An empty filter means every subject of the stream.
fn consumer_overlaps(consumer: &jetstream::consumer::Config, pattern: &str) -> bool {
    let filters = consumer
        .filter_subjects
        .iter()
        .chain((!consumer.filter_subject.is_empty()).then_some(&consumer.filter_subject));
    let mut filters = filters.peekable();
    if filters.peek().is_none() {
        return true;
    }
    filters.any(|f| subjects_overlap(f, pattern))
}

/// Configs of the stream's consumers other than `own`.
async fn other_consumers(
    stream: &jetstream::stream::Stream,
    own: &str,
) -> anyhow::Result<Vec<jetstream::consumer::Config>> {
    use futures_util::StreamExt as _;
    let mut infos = stream.consumers();
    let mut others = Vec::new();
    while let Some(info) = infos.next().await {
        let info = info.context("listing the stream's consumers")?;
        if info.name != own {
            others.push(info.config);
        }
    }
    Ok(others)
}

fn schedule_subject_filter(subject: &str) -> String {
    format!("{subject}.scheduled.*")
}

fn at_least_2_12(version: &str) -> bool {
    let mut parts = version
        .split(|c: char| !c.is_ascii_digit())
        .filter(|p| !p.is_empty())
        .map(|p| p.parse::<u32>().unwrap_or(0));
    (parts.next().unwrap_or(0), parts.next().unwrap_or(0)) >= (2, 12)
}

/// Why schedules cannot be used with the stream, consumer and server as observed, if they cannot.
fn scheduling_blocker(
    stream: &jetstream::stream::Config,
    consumer: &jetstream::consumer::Config,
    others: &[jetstream::consumer::Config],
    subject: &str,
    server_version: &str,
    deadline: std::time::Duration,
) -> Option<&'static str> {
    let filters_only_subject = if consumer.filter_subjects.is_empty() {
        consumer.filter_subject == subject
    } else {
        consumer.filter_subject.is_empty() && consumer.filter_subjects == [subject]
    };
    if !filters_only_subject {
        Some("the consumer does not filter exactly the queued subject")
    } else if !matches!(
        stream.retention,
        jetstream::stream::RetentionPolicy::WorkQueue | jetstream::stream::RetentionPolicy::Limits
    ) {
        Some("the stream retention is not WorkQueue or Limits")
    } else if stream.mirror.is_some() || stream.sources.as_ref().is_some_and(|s| !s.is_empty()) {
        Some("the stream is a mirror or has sources")
    } else if stream.discard != jetstream::stream::DiscardPolicy::Old {
        Some("the stream discard policy is not Old")
    } else if stream.subject_transform.is_some() {
        Some("the stream has a subject transform")
    } else if !stream.max_age.is_zero() && stream.max_age <= SCHEDULE_MARGIN {
        Some("the stream max_age is too short to hold schedules")
    } else if others
        .iter()
        .any(|c| consumer_overlaps(c, &schedule_subject_filter(subject)))
    {
        Some("another consumer reads the schedule subjects")
    } else if !at_least_2_12(server_version) {
        Some("the NATS server is older than 2.12")
    } else if deadline < std::time::Duration::from_secs(1) {
        Some("the consumer ack deadline is under one second")
    } else {
        None
    }
}

fn has_schedules(config: &jetstream::stream::Config, subject: &str) -> bool {
    config.allow_message_schedules && config.subjects.contains(&schedule_subject_filter(subject))
}

/// Adds the schedule subject and flag to the stream when missing. The flag cannot be removed
/// once set. Returns whether the stream ends up with both.
async fn enable_schedules(
    js: &jetstream::Context,
    observed: &jetstream::stream::Config,
    subject: &str,
) -> bool {
    if has_schedules(observed, subject) {
        return true;
    }
    let mut config = observed.clone();
    let filter = schedule_subject_filter(subject);
    if !config.subjects.contains(&filter) {
        config.subjects.push(filter);
    }
    config.allow_message_schedules = true;
    match js.update_stream(&config).await {
        Ok(info) if has_schedules(&info.config, subject) => true,
        Ok(_) => {
            tracing::warn!("the stream update did not enable message schedules");
            false
        }
        Err(e) => {
            tracing::warn!(error = %e, "could not enable message schedules on the stream");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use async_nats::jetstream::consumer::Config as ConsumerConfig;
    use async_nats::jetstream::stream::{Config, DiscardPolicy, RetentionPolicy};

    use super::{
        at_least_2_12, consumer_overlaps, effective_deadline, schedule_limit, scheduling_blocker,
        subjects_overlap,
    };

    #[test]
    fn an_ack_wait_of_zero_means_the_thirty_second_default() {
        assert_eq!(
            effective_deadline(Duration::ZERO, &[]),
            Duration::from_secs(30)
        );
    }

    #[test]
    fn the_shortest_backoff_entry_bounds_the_deadline() {
        let backoff = [Duration::from_mins(1), Duration::from_millis(1_500)];

        assert_eq!(
            effective_deadline(Duration::from_secs(30), &backoff),
            Duration::from_millis(1_500)
        );
    }

    #[test]
    fn ack_wait_bounds_the_deadline_when_it_is_the_smallest() {
        assert_eq!(
            effective_deadline(Duration::from_secs(3), &[Duration::from_secs(30)]),
            Duration::from_secs(3)
        );
    }

    #[test]
    fn server_versions_are_compared_leniently() {
        assert!(at_least_2_12("2.12.0"));
        assert!(at_least_2_12("2.15.3"));
        assert!(at_least_2_12("2.12.0-RC.1"));
        assert!(at_least_2_12("3.0.0"));
        assert!(!at_least_2_12("2.11.9"));
        assert!(!at_least_2_12("2.9.0"));
        assert!(!at_least_2_12(""));
    }

    fn consumer(subject: &str) -> ConsumerConfig {
        ConsumerConfig {
            filter_subject: subject.to_owned(),
            ..Default::default()
        }
    }

    fn blocker(
        stream: &Config,
        consumer: &ConsumerConfig,
        version: &str,
        deadline: Duration,
    ) -> Option<&'static str> {
        scheduling_blocker(stream, consumer, &[], "q.queued", version, deadline)
    }

    #[test]
    fn a_matching_setup_has_no_blocker() {
        let stream = Config::default();

        assert_eq!(
            blocker(
                &stream,
                &consumer("q.queued"),
                "2.12.0",
                Duration::from_secs(1)
            ),
            None
        );
        let multi = ConsumerConfig {
            filter_subjects: vec!["q.queued".to_owned()],
            ..Default::default()
        };
        assert_eq!(
            blocker(&stream, &multi, "2.12.0", Duration::from_secs(5)),
            None
        );
    }

    #[test]
    fn each_unsuitable_setting_blocks_scheduling() {
        let ok = Config::default();
        let deadline = Duration::from_secs(5);

        assert!(blocker(&ok, &ConsumerConfig::default(), "2.12.0", deadline).is_some());
        assert!(blocker(&ok, &consumer("q.>"), "2.12.0", deadline).is_some());
        let wider = ConsumerConfig {
            filter_subjects: vec!["q.queued".to_owned(), "q.other".to_owned()],
            ..Default::default()
        };
        assert!(blocker(&ok, &wider, "2.12.0", deadline).is_some());
        let interest = Config {
            retention: RetentionPolicy::Interest,
            ..Default::default()
        };
        assert!(blocker(&interest, &consumer("q.queued"), "2.12.0", deadline).is_some());
        let discard_new = Config {
            discard: DiscardPolicy::New,
            ..Default::default()
        };
        assert!(blocker(&discard_new, &consumer("q.queued"), "2.12.0", deadline).is_some());
        assert!(blocker(&ok, &consumer("q.queued"), "2.11.0", deadline).is_some());
        assert!(
            blocker(
                &ok,
                &consumer("q.queued"),
                "2.12.0",
                Duration::from_millis(999)
            )
            .is_some()
        );
    }

    #[test]
    fn a_stream_subject_transform_blocks_scheduling() {
        let transformed = Config {
            subject_transform: Some(async_nats::jetstream::stream::SubjectTransform {
                source: "q.>".to_owned(),
                destination: "z.>".to_owned(),
            }),
            ..Default::default()
        };

        assert!(
            blocker(
                &transformed,
                &consumer("q.queued"),
                "2.12.0",
                Duration::from_secs(5)
            )
            .is_some()
        );
    }

    #[test]
    fn a_short_max_age_blocks_scheduling_and_a_long_one_caps_the_horizon() {
        let short = Config {
            max_age: Duration::from_mins(5),
            ..Default::default()
        };
        let long = Config {
            max_age: Duration::from_hours(1),
            ..Default::default()
        };
        let deadline = Duration::from_secs(5);

        assert!(blocker(&short, &consumer("q.queued"), "2.12.0", deadline).is_some());
        assert_eq!(
            blocker(&long, &consumer("q.queued"), "2.12.0", deadline),
            None
        );
        assert_eq!(schedule_limit(Duration::ZERO), None);
        assert_eq!(
            schedule_limit(Duration::from_hours(1)),
            Some(Duration::from_mins(55))
        );
    }

    #[test]
    fn subject_overlap_handles_exact_names_and_wildcards() {
        let pattern = "q.queued.scheduled.*";
        assert!(subjects_overlap("q.queued.scheduled.*", pattern));
        assert!(subjects_overlap("q.queued.scheduled.abc", pattern));
        assert!(subjects_overlap("q.queued.*.*", pattern));
        assert!(subjects_overlap("q.>", pattern));
        assert!(subjects_overlap(">", pattern));
        assert!(!subjects_overlap("q.queued", pattern));
        assert!(!subjects_overlap("q.other.>", pattern));
        assert!(!subjects_overlap("q.queued.scheduled", pattern));
        assert!(!subjects_overlap("q.queued.scheduled.a.b", pattern));
    }

    #[test]
    fn an_unfiltered_consumer_overlaps_every_subject() {
        assert!(consumer_overlaps(
            &ConsumerConfig::default(),
            "q.queued.scheduled.*"
        ));
        assert!(consumer_overlaps(&consumer("q.>"), "q.queued.scheduled.*"));
        assert!(!consumer_overlaps(
            &consumer("q.queued"),
            "q.queued.scheduled.*"
        ));
        let listed = ConsumerConfig {
            filter_subjects: vec!["q.queued".to_owned(), "q.queued.scheduled.7".to_owned()],
            ..Default::default()
        };
        assert!(consumer_overlaps(&listed, "q.queued.scheduled.*"));
    }

    #[test]
    fn another_consumer_on_the_schedule_subjects_blocks_scheduling() {
        let others = [consumer("q.queued.scheduled.*")];

        let result = scheduling_blocker(
            &Config::default(),
            &consumer("q.queued"),
            &others,
            "q.queued",
            "2.12.0",
            Duration::from_secs(5),
        );

        assert!(result.is_some());
    }
}
