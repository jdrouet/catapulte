//! A worker that dies right after removing the queue row must still leave the email finished:
//! the terminal event has to reach storage in the same commit as the removal.

use std::sync::Arc;
use std::time::Duration;

use catapulte_domain::entity::body::{BodySource, Plain};
use catapulte_domain::entity::email::{EmailId, RecipientKind};
use catapulte_domain::entity::envelope::Envelope;
use catapulte_domain::entity::lifecycle_event::LifecycleEvent;
use catapulte_domain::entity::sender::SenderName;
use catapulte_domain::port::email_queue::EmailQueue;
use catapulte_domain::port::email_repository::{EmailRepository, EmailStatus, ListEmailsParams};
use catapulte_domain::port::event_publisher::{EventPublisher, EventPublisherError};
use catapulte_domain::port::event_repository::{EventRepository, ListEventsParams};
use catapulte_domain::use_case::process_queued_email::{
    ProcessQueuedEmailError, ProcessQueuedEmailUseCase, ProcessedEmail,
};
use catapulte_inbound_worker::worker::{WorkerConfig, WorkerState};
use catapulte_outbound_attachment_fs::store::FsAttachmentStore;
use catapulte_outbound_sqlite::SqliteAdapter;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy)]
enum Outcome {
    Sent,
    PermanentFailure,
}

#[derive(Clone)]
struct StubProcessor(Outcome);

impl ProcessQueuedEmailUseCase for StubProcessor {
    async fn execute(
        &self,
        _: EmailId,
        _: Envelope,
    ) -> Result<ProcessedEmail, ProcessQueuedEmailError> {
        match self.0 {
            Outcome::Sent => Ok(ProcessedEmail {
                sender_name: SenderName::new("primary"),
                smtp_response: None,
                message_id: "<m@example.com>".to_owned(),
                suppressed: vec![],
            }),
            Outcome::PermanentFailure => Err(ProcessQueuedEmailError::AllRecipientsSuppressed),
        }
    }
}

/// Stands for the process dying before a terminal event lands: its `publish` never returns.
#[derive(Clone)]
struct CrashingPublisher {
    inner: SqliteAdapter,
    reached: Arc<Notify>,
}

impl EventPublisher for CrashingPublisher {
    async fn publish(&self, event: &LifecycleEvent) -> Result<(), EventPublisherError> {
        if matches!(event.event_type(), "delivery.succeeded" | "delivery.failed") {
            self.reached.notify_one();
            std::future::pending::<()>().await;
        }
        self.inner.publish(event).await
    }
}

#[derive(Clone)]
struct TestState {
    adapter: SqliteAdapter,
    publisher: CrashingPublisher,
    store: FsAttachmentStore,
    processor: StubProcessor,
    _dir: Arc<tempfile::TempDir>,
}

impl WorkerState for TestState {
    fn process_queued_email(&self) -> &impl ProcessQueuedEmailUseCase {
        &self.processor
    }

    fn email_queue(&self) -> &impl EmailQueue {
        &self.adapter
    }

    fn event_publisher(&self) -> &impl EventPublisher {
        &self.publisher
    }

    fn attachment_store(&self) -> &impl catapulte_domain::port::attachment_store::AttachmentStore {
        &self.store
    }

    fn email_repository(
        &self,
    ) -> &(impl EmailRepository + catapulte_domain::port::email_repository::EmailCancellation) {
        &self.adapter
    }
}

async fn crash_during_terminal_publish(outcome: Outcome) -> (SqliteAdapter, EmailId) {
    let adapter = SqliteAdapter::connect(":memory:").await.unwrap();
    adapter.migrate().await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let reached = Arc::new(Notify::new());
    let state = TestState {
        adapter: adapter.clone(),
        publisher: CrashingPublisher {
            inner: adapter.clone(),
            reached: reached.clone(),
        },
        store: FsAttachmentStore::new(dir.path().to_path_buf())
            .await
            .unwrap(),
        processor: StubProcessor(outcome),
        _dir: Arc::new(dir),
    };

    let id = EmailId::default();
    let envelope = Envelope::new(
        "sender@example.com",
        vec![(RecipientKind::To, "to@example.com".to_owned())],
        BodySource::Plain(Plain::try_new(Some("hello".to_owned()), None).unwrap()),
    );
    adapter.save(id, &envelope).await.unwrap();
    adapter.enqueue(id, &envelope).await.unwrap();

    let handle = tokio::spawn(
        WorkerConfig::default()
            .build()
            .run(state, CancellationToken::new()),
    );
    tokio::time::timeout(Duration::from_secs(5), reached.notified())
        .await
        .expect("the worker never reached the terminal publish");
    handle.abort();
    let _ = handle.await;
    (adapter, id)
}

async fn assert_finished_once(
    adapter: &SqliteAdapter,
    id: EmailId,
    status: EmailStatus,
    event: &str,
) {
    let emails = adapter
        .list_emails(ListEmailsParams {
            tag: None,
            owner: None,
            status: Some(status),
            after_ms: None,
            before_ms: None,
            recipient: None,
            template: None,
            id: Some(id),
            limit: 10,
            offset: 0,
        })
        .await
        .unwrap();
    assert_eq!(emails.len(), 1, "the email is not {status:?}");

    let events = adapter
        .list_events(ListEventsParams {
            owner: None,
            email_id: Some(id),
            event_type: Some(event.to_owned()),
            sender_name: None,
            error_class: None,
            after_ms: None,
            before_ms: None,
            limit: 10,
            offset: 0,
        })
        .await
        .unwrap();
    assert_eq!(events.len(), 1, "{events:?}");
}

#[tokio::test]
async fn a_crash_after_a_send_still_leaves_the_email_finished() {
    let (adapter, id) = crash_during_terminal_publish(Outcome::Sent).await;
    assert_finished_once(&adapter, id, EmailStatus::Sent, "delivery.succeeded").await;
}

#[tokio::test]
async fn a_crash_after_a_permanent_failure_still_leaves_the_email_finished() {
    let (adapter, id) = crash_during_terminal_publish(Outcome::PermanentFailure).await;
    assert_finished_once(&adapter, id, EmailStatus::Failed, "delivery.failed").await;
}
