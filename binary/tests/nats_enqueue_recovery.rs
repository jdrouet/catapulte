//! Recovery of emails whose NATS publish ack was lost. The wrapper queue performs the real
//! acked publish and then reports `Uncertain` once, so the retry path is exercised
//! deterministically. The test drives the dequeue itself instead of running a worker.
//! Delivery stays at least once; nothing here claims exactly-once.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use async_nats::jetstream;
use catapulte_domain::entity::attachment::{AttachmentRef, BlobRef};
use catapulte_domain::entity::body::{BodySource, Plain};
use catapulte_domain::entity::email::{EmailId, RecipientKind};
use catapulte_domain::entity::envelope::Envelope;
use catapulte_domain::entity::lifecycle_event::LifecycleEvent;
use catapulte_domain::port::email_queue::{AckToken, DequeuedEmail, EmailQueue, EmailQueueError};
use catapulte_domain::port::email_repository::{
    DuplicateState, EmailRecord, EmailRepository, EmailRepositoryError, EnqueueState,
    ListEmailsParams, SaveResult,
};
use catapulte_domain::port::event_publisher::EventPublisher;
use catapulte_domain::use_case::submit_email::{
    AttachmentInput, SubmitEmailInput, SubmitEmailService,
};
use catapulte_outbound_attachment_fetcher::fetcher::HttpAttachmentFetcherConfig;
use catapulte_outbound_attachment_fs::store::FsAttachmentStore;
use catapulte_outbound_nats::{NatsAdapter, NatsConfig};
use catapulte_outbound_sqlite::SqliteAdapter;
use testcontainers::GenericImage;
use testcontainers::ImageExt;
use testcontainers::core::{IntoContainerPort, WaitFor};
use testcontainers::runners::AsyncRunner;

const STREAM: &str = "RECOVERY";

/// Real acked publish, then `Uncertain` once while `lose_ack` is set.
#[derive(Clone)]
struct AckLosingQueue {
    inner: NatsAdapter,
    lose_ack: Arc<AtomicBool>,
    publishes: Arc<AtomicUsize>,
    last_envelope: Arc<std::sync::Mutex<Option<Envelope>>>,
}

impl EmailQueue for AckLosingQueue {
    async fn enqueue(&self, id: EmailId, envelope: &Envelope) -> Result<(), EmailQueueError> {
        self.inner.enqueue(id, envelope).await?;
        self.publishes.fetch_add(1, Ordering::SeqCst);
        *self.last_envelope.lock().unwrap() = Some(envelope.clone());
        if self.lose_ack.swap(false, Ordering::SeqCst) {
            return Err(EmailQueueError::Uncertain {
                source: anyhow::anyhow!("injected: ack lost after a successful publish"),
            });
        }
        Ok(())
    }

    async fn dequeue(&self) -> Result<DequeuedEmail, EmailQueueError> {
        self.inner.dequeue().await
    }

    async fn ack(&self, token: AckToken) -> Result<(), EmailQueueError> {
        self.inner.ack(token).await
    }

    async fn nack(&self, token: AckToken, delay: Duration) -> Result<(), EmailQueueError> {
        self.inner.nack(token, delay).await
    }
}

/// Sqlite repository whose `Enqueued` state write can be made to fail.
#[derive(Clone)]
struct StateFailingRepository {
    inner: SqliteAdapter,
    fail_enqueued: Arc<AtomicBool>,
}

impl EmailRepository for StateFailingRepository {
    async fn save(
        &self,
        id: EmailId,
        envelope: &Envelope,
    ) -> Result<SaveResult, EmailRepositoryError> {
        self.inner.save(id, envelope).await
    }

    async fn list_all_attachment_blobs(&self) -> Result<Vec<BlobRef>, EmailRepositoryError> {
        self.inner.list_all_attachment_blobs().await
    }

    async fn list_emails(
        &self,
        params: ListEmailsParams,
    ) -> Result<Vec<EmailRecord>, EmailRepositoryError> {
        self.inner.list_emails(params).await
    }

    async fn set_attachments(
        &self,
        id: EmailId,
        attachments: &[AttachmentRef],
    ) -> Result<(), EmailRepositoryError> {
        self.inner.set_attachments(id, attachments).await
    }

    async fn set_enqueue_state(
        &self,
        id: EmailId,
        state: EnqueueState,
    ) -> Result<(), EmailRepositoryError> {
        if state == EnqueueState::Enqueued && self.fail_enqueued.load(Ordering::SeqCst) {
            return Err(EmailRepositoryError::Storage {
                source: anyhow::anyhow!("injected: enqueued state write failed"),
            });
        }
        self.inner.set_enqueue_state(id, state).await
    }

    async fn delete(&self, id: EmailId) -> Result<(), EmailRepositoryError> {
        self.inner.delete(id).await
    }
}

type Service = SubmitEmailService<
    StateFailingRepository,
    AckLosingQueue,
    SqliteAdapter,
    FsAttachmentStore,
    catapulte_outbound_attachment_fetcher::fetcher::HttpAttachmentFetcher,
>;

struct Harness {
    service: Service,
    queue: AckLosingQueue,
    repo: StateFailingRepository,
    events: SqliteAdapter,
    stream: jetstream::stream::Stream,
    _nats: testcontainers::ContainerAsync<GenericImage>,
    _dir: tempfile::TempDir,
}

async fn harness() -> Harness {
    let nats = GenericImage::new("nats", "2-alpine")
        .with_exposed_port(4222.tcp())
        .with_wait_for(WaitFor::message_on_stderr("Server is ready"))
        .with_wait_for(WaitFor::millis(200))
        .with_cmd(["nats-server".to_owned(), "--js".to_owned()])
        .start()
        .await
        .expect("failed to start NATS container; ensure Docker is running");
    let port = nats.get_host_port_ipv4(4222).await.unwrap();
    let url = format!("nats://127.0.0.1:{port}");

    let adapter = NatsConfig {
        url: url.clone(),
        stream: STREAM.to_owned(),
        subject: "recovery.queued".to_owned(),
        consumer: "worker".to_owned(),
        ack_wait_secs: 30,
        max_deliver: 3,
        backoff_secs: vec![30, 60, 120],
    }
    .build()
    .await
    .expect("failed to build NATS adapter");

    let client = async_nats::connect(&url).await.unwrap();
    let stream = jetstream::new(client).get_stream(STREAM).await.unwrap();

    let sqlite = SqliteAdapter::connect(":memory:").await.unwrap();
    sqlite.migrate().await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let store = FsAttachmentStore::new(dir.path().to_path_buf())
        .await
        .unwrap();

    let queue = AckLosingQueue {
        inner: adapter,
        lose_ack: Arc::new(AtomicBool::new(false)),
        publishes: Arc::new(AtomicUsize::new(0)),
        last_envelope: Arc::new(std::sync::Mutex::new(None)),
    };
    let repo = StateFailingRepository {
        inner: sqlite.clone(),
        fail_enqueued: Arc::new(AtomicBool::new(false)),
    };
    let service = SubmitEmailService::new(
        repo.clone(),
        queue.clone(),
        sqlite.clone(),
        store,
        HttpAttachmentFetcherConfig::default().build().unwrap(),
    );
    Harness {
        service,
        queue,
        repo,
        events: sqlite,
        stream,
        _nats: nats,
        _dir: dir,
    }
}

fn input_with(
    subject: &str,
    sender: &str,
    recipient: &str,
    body: &str,
    correlation_id: &str,
    attachment: &[u8],
) -> SubmitEmailInput {
    let mut variables = serde_json::Map::new();
    variables.insert("name".to_owned(), serde_json::json!(subject));
    SubmitEmailInput {
        idempotency_key: Some("recovery-key".to_owned()),
        correlation_id: Some(correlation_id.to_owned()),
        subject: Some(subject.to_owned()),
        sender: sender.to_owned(),
        recipients: vec![(RecipientKind::To, recipient.to_owned())],
        body: BodySource::Plain(Plain::try_new(Some(body.to_owned()), None).unwrap()),
        variables,
        attachments: vec![AttachmentInput::Stream {
            filename: "a.txt".to_owned(),
            content_type: "text/plain".to_owned(),
            reader: Box::pin(std::io::Cursor::new(attachment.to_vec())),
        }],
        reply_to: vec![format!("Reply <{correlation_id}@reply.example.com>")],
        headers: vec![("X-Corr".to_owned(), correlation_id.to_owned())],
    }
}

fn input() -> SubmitEmailInput {
    input_with(
        "hello",
        "sender@example.com",
        "to@example.com",
        "body",
        "corr-1",
        b"attachment",
    )
}

/// Same idempotency key, different content in every field.
fn different_input() -> SubmitEmailInput {
    input_with(
        "other",
        "other@example.com",
        "else@example.com",
        "another body",
        "corr-2",
        b"different attachment",
    )
}

impl Harness {
    async fn stream_messages(&mut self) -> u64 {
        self.stream.info().await.unwrap().state.messages
    }

    async fn dequeue(&self) -> DequeuedEmail {
        tokio::time::timeout(Duration::from_secs(10), self.queue.dequeue())
            .await
            .expect("dequeue timed out")
            .unwrap()
    }

    /// True when a same-key save finds an already-enqueued row with nothing to replay.
    async fn row_is_enqueued(&self) -> bool {
        let probe = Envelope {
            idempotency_key: Some("recovery-key".to_owned()),
            ..Envelope::new(
                "probe@example.com",
                vec![],
                BodySource::Plain(Plain::try_new(Some("x".to_owned()), None).unwrap()),
            )
        };
        matches!(
            self.repo.save(EmailId::default(), &probe).await.unwrap(),
            SaveResult::Duplicate {
                state: DuplicateState::Complete,
                recovery: None,
                ..
            }
        )
    }
}

fn assert_same_envelope(got: &Envelope, published: &Envelope) {
    assert_eq!(got.idempotency_key, published.idempotency_key);
    assert_eq!(got.correlation_id, published.correlation_id);
    assert_eq!(got.subject, published.subject);
    assert_eq!(got.sender, published.sender);
    assert_eq!(got.recipients, published.recipients);
    assert_eq!(format!("{:?}", got.body), format!("{:?}", published.body));
    assert_eq!(got.variables, published.variables);
    assert_eq!(got.attachments, published.attachments);
    assert_eq!(got.reply_to, published.reply_to);
    assert_eq!(got.headers, published.headers);
}

#[tokio::test]
async fn window_is_the_nats_default_of_two_minutes() {
    let mut h = harness().await;
    let window = h.stream.info().await.unwrap().config.duplicate_window;
    assert_eq!(window, Duration::from_mins(2));
}

#[tokio::test]
async fn lost_ack_then_retry_before_any_worker() {
    let mut h = harness().await;
    h.queue.lose_ack.store(true, Ordering::SeqCst);

    h.service.execute(input()).await.unwrap_err();
    let id = h.service.execute(input()).await.unwrap();

    assert_eq!(h.queue.publishes.load(Ordering::SeqCst), 2);
    assert_eq!(h.stream_messages().await, 1, "deduplicated by Nats-Msg-Id");
    let got = h.dequeue().await;
    assert_eq!(got.id, id);
    let published = h.queue.last_envelope.lock().unwrap().clone().unwrap();
    assert_same_envelope(&got.envelope, &published);
    assert_eq!(got.envelope.attachments.len(), 1);
    let second = tokio::time::timeout(Duration::from_secs(2), h.queue.dequeue()).await;
    assert!(
        second.is_err(),
        "exactly one message must be delivered; got {:?}",
        second.map(|r| r.map(|d| d.attempt))
    );
    assert!(h.row_is_enqueued().await);
}

#[tokio::test]
async fn failed_post_publish_state_write_retry_republishes_without_duplicating() {
    let mut h = harness().await;
    h.repo.fail_enqueued.store(true, Ordering::SeqCst);

    let id = h.service.execute(input()).await.unwrap();
    assert!(!h.row_is_enqueued().await);
    let again = h.service.execute(input()).await.unwrap();

    assert_eq!(again, id);
    assert_eq!(h.queue.publishes.load(Ordering::SeqCst), 2);
    assert_eq!(h.stream_messages().await, 1);
}

#[tokio::test]
async fn worker_started_and_published_sending_so_retry_does_not_republish() {
    let mut h = harness().await;
    h.queue.lose_ack.store(true, Ordering::SeqCst);
    h.service.execute(input()).await.unwrap_err();

    let got = h.dequeue().await;
    h.events
        .publish(&LifecycleEvent::Sending {
            id: got.id,
            attempt: 1,
            correlation_id: None,
        })
        .await
        .unwrap();
    let id = h.service.execute(input()).await.unwrap();

    assert_eq!(id, got.id);
    assert_eq!(h.queue.publishes.load(Ordering::SeqCst), 1, "no republish");
    assert_eq!(h.stream_messages().await, 1);
    assert!(h.row_is_enqueued().await);
}

#[tokio::test]
async fn worker_started_without_sending_event_so_retry_republishes_and_is_deduplicated() {
    let mut h = harness().await;
    h.queue.lose_ack.store(true, Ordering::SeqCst);
    h.service.execute(input()).await.unwrap_err();

    let got = h.dequeue().await;
    let id = h.service.execute(input()).await.unwrap();

    assert_eq!(id, got.id);
    assert_eq!(
        h.queue.publishes.load(Ordering::SeqCst),
        2,
        "the sending-event evidence is advisory"
    );
    assert_eq!(h.stream_messages().await, 1, "dedup within the window");
}

#[tokio::test]
async fn replay_uses_the_persisted_payload_not_the_retry_content() {
    let mut h = harness().await;
    h.queue.lose_ack.store(true, Ordering::SeqCst);
    h.service.execute(input()).await.unwrap_err();
    let original = h.queue.last_envelope.lock().unwrap().clone().unwrap();

    let id = h.service.execute(different_input()).await.unwrap();

    assert_eq!(h.stream_messages().await, 1);
    let replayed = h.queue.last_envelope.lock().unwrap().clone().unwrap();
    assert_same_envelope(&replayed, &original);
    let got = h.dequeue().await;
    assert_eq!(got.id, id);
    assert_same_envelope(&got.envelope, &original);
    assert_eq!(got.envelope.sender, "sender@example.com");
}
