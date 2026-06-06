use catapulte_domain::entity::lifecycle_event::LifecycleEvent;
use catapulte_domain::port::attachment_store::AttachmentStore;
use catapulte_domain::port::email_queue::{AckToken, EmailQueue};
use catapulte_domain::port::email_repository::EmailRepository;
use catapulte_domain::port::event_publisher::EventPublisher;
use catapulte_domain::use_case::process_queued_email::{
    ProcessQueuedEmailError, ProcessQueuedEmailUseCase,
};
use tracing::Instrument as _;

const MAX_ATTEMPTS: u32 = 3;

pub trait WorkerState: Clone + Send + Sync + 'static {
    fn process_queued_email(&self) -> &impl ProcessQueuedEmailUseCase;
    fn email_queue(&self) -> &impl EmailQueue;
    fn event_publisher(&self) -> &impl EventPublisher;
    fn attachment_store(&self) -> &impl AttachmentStore;
    fn email_repository(&self) -> &impl EmailRepository;
}

pub struct WorkerConfig {}

impl WorkerConfig {
    /// # Errors
    ///
    /// Always succeeds; the signature is kept for consistency with other configs.
    pub fn from_env(_prefix: &str) -> anyhow::Result<Self> {
        Ok(Self {})
    }

    #[must_use]
    pub fn build(self) -> Worker {
        Worker {}
    }
}

pub struct Worker {}

fn backoff(attempt: u32) -> std::time::Duration {
    let secs = (30u64 * (1u64 << attempt.saturating_sub(1))).min(3600);
    std::time::Duration::from_secs(secs)
}

impl Worker {
    pub async fn run<S: WorkerState>(self, state: S, cancel: tokio_util::sync::CancellationToken) {
        loop {
            let result = tokio::select! {
                biased;
                () = cancel.cancelled() => break,
                result = state.email_queue().dequeue() => result,
            };

            match result {
                Ok(dequeued) => {
                    if cancel.is_cancelled() {
                        if let Err(e) = state
                            .email_queue()
                            .nack(dequeued.token, std::time::Duration::ZERO)
                            .await
                        {
                            tracing::error!(error = %e, "failed to nack message on shutdown");
                        }
                        break;
                    }
                    process_one(
                        &state,
                        dequeued.id,
                        dequeued.envelope,
                        dequeued.attempt,
                        dequeued.token,
                        dequeued.trace,
                    )
                    .await;
                }
                Err(e) => {
                    tracing::error!(error = %e, "failed to dequeue");
                    tokio::select! {
                        biased;
                        () = cancel.cancelled() => break,
                        () = tokio::time::sleep(std::time::Duration::from_secs(1)) => {}
                    }
                }
            }
        }
        tracing::info!("worker stopped");
    }
}

#[allow(clippy::too_many_lines)]
async fn process_one<S: WorkerState>(
    state: &S,
    id: catapulte_domain::entity::email::EmailId,
    envelope: catapulte_domain::entity::envelope::Envelope,
    attempt: u32,
    token: AckToken,
    trace: catapulte_domain::port::email_queue::TraceCarrier,
) {
    let span = tracing::info_span!(
        "worker.process",
        email_id = %id.as_uuid(),
        attempt,
        correlation_id = tracing::field::Empty,
    );
    catapulte_telemetry::propagation::set_span_parent(&span, trace.as_pairs());

    async move {
        let correlation_id = envelope.correlation_id.clone();
        if let Some(ref cid) = correlation_id {
            tracing::Span::current().record("correlation_id", cid.as_str());
        }

        if let Err(e) = state
            .event_publisher()
            .publish(&LifecycleEvent::Sending {
                id,
                attempt,
                correlation_id: correlation_id.clone(),
            })
            .await
        {
            tracing::error!(error = %e, "failed to publish sending event");
        }

        let attachments_for_cleanup = envelope.attachments.clone();

        match state.process_queued_email().execute(envelope).await {
            Ok(sender_name) => {
                if let Err(e) = state.email_queue().ack(token).await {
                    tracing::error!(error = %e, email_id = %id.as_uuid(), "failed to ack email");
                    return;
                }
                if let Err(e) = state.email_repository().set_attachments(id, &[]).await {
                    tracing::warn!(
                        error = %e,
                        email_id = %id.as_uuid(),
                        "failed to clear attachment refs after send"
                    );
                }
                for att in &attachments_for_cleanup {
                    if let Err(e) = state.attachment_store().delete(&att.blob).await {
                        tracing::warn!(
                            error = %e,
                            blob_key = %att.blob.key,
                            "failed to delete attachment blob after send"
                        );
                    }
                }
                if let Err(e) = state
                    .event_publisher()
                    .publish(&LifecycleEvent::Sent {
                        id,
                        sender_name,
                        correlation_id: correlation_id.clone(),
                    })
                    .await
                {
                    tracing::error!(error = %e, "failed to publish sent event");
                }
            }
            Err(e) => {
                tracing::error!(
                    error = ?e,
                    email_id = %id.as_uuid(),
                    attempt,
                    "failed to process email"
                );
                let reason = e.to_string();
                let sender_name = e.sender_name().cloned();
                let is_terminal =
                    matches!(&e, ProcessQueuedEmailError::Send(s) if !s.is_transient())
                        || attempt >= MAX_ATTEMPTS;
                let event = if is_terminal {
                    if let Err(ack_err) = state.email_queue().ack(token).await {
                        tracing::error!(error = %ack_err, "failed to ack permanently failed email");
                        return;
                    }
                    LifecycleEvent::Failed {
                        id,
                        attempt,
                        reason,
                        sender_name,
                        correlation_id: correlation_id.clone(),
                    }
                } else {
                    let delay = backoff(attempt);
                    if let Err(nack_err) = state.email_queue().nack(token, delay).await {
                        tracing::error!(
                            error = %nack_err,
                            "failed to nack transiently failed email"
                        );
                        return;
                    }
                    LifecycleEvent::Retrying {
                        id,
                        attempt,
                        reason,
                        sender_name,
                        correlation_id: correlation_id.clone(),
                    }
                };
                if let Err(pub_err) = state.event_publisher().publish(&event).await {
                    tracing::error!(error = %pub_err, "failed to publish lifecycle event");
                }
            }
        }
    }
    .instrument(span)
    .await;
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use catapulte_domain::entity::attachment::{AttachmentRef, BlobRef};
    use catapulte_domain::entity::body::{BodySource, Plain};
    use catapulte_domain::entity::email::{EmailId, RecipientKind};
    use catapulte_domain::entity::envelope::Envelope;
    use catapulte_domain::entity::lifecycle_event::LifecycleEvent;
    use catapulte_domain::entity::sender::SenderName;
    use catapulte_domain::port::attachment_store::{
        AttachmentReader, AttachmentStore, AttachmentStoreError, PutResult,
    };
    use catapulte_domain::port::email_queue::{
        AckToken, DequeuedEmail, EmailQueue, EmailQueueError, TraceCarrier,
    };
    use catapulte_domain::port::email_repository::{
        EmailRecord, EmailRepository, EmailRepositoryError, ListEmailsParams, SaveResult,
    };
    use catapulte_domain::port::event_publisher::{EventPublisher, EventPublisherError};
    use catapulte_domain::use_case::process_queued_email::{
        ProcessQueuedEmailError, ProcessQueuedEmailUseCase,
    };

    use super::{Worker, WorkerState, process_one};

    fn sample_envelope() -> Envelope {
        Envelope {
            idempotency_key: None,
            correlation_id: None,
            subject: None,
            sender: "sender@example.com".to_owned(),
            recipients: vec![(RecipientKind::To, "to@example.com".to_owned())],
            body: BodySource::Plain(Plain::try_new(Some("hi".to_owned()), None).unwrap()),
            variables: serde_json::Map::new(),
            attachments: vec![],
        }
    }

    struct OkProcessor;

    impl ProcessQueuedEmailUseCase for OkProcessor {
        async fn execute(&self, _: Envelope) -> Result<SenderName, ProcessQueuedEmailError> {
            Ok(SenderName::new("sender"))
        }
    }

    #[derive(Clone)]
    struct FailingAckQueue;

    impl EmailQueue for FailingAckQueue {
        async fn enqueue(&self, _: EmailId, _: &Envelope) -> Result<(), EmailQueueError> {
            Ok(())
        }

        async fn dequeue(&self) -> Result<DequeuedEmail, EmailQueueError> {
            unimplemented!()
        }

        async fn ack(&self, _: AckToken) -> Result<(), EmailQueueError> {
            Err(EmailQueueError::Storage {
                source: anyhow::anyhow!("ack failed"),
            })
        }

        async fn nack(&self, _: AckToken, _: Duration) -> Result<(), EmailQueueError> {
            Ok(())
        }
    }

    #[derive(Clone, Default)]
    struct RecordingPublisher {
        events: Arc<Mutex<Vec<&'static str>>>,
    }

    impl EventPublisher for RecordingPublisher {
        async fn publish(&self, event: &LifecycleEvent) -> Result<(), EventPublisherError> {
            let kind = match event {
                LifecycleEvent::Queued { .. } => "queued",
                LifecycleEvent::Sending { .. } => "sending",
                LifecycleEvent::Sent { .. } => "sent",
                LifecycleEvent::Failed { .. } => "failed",
                LifecycleEvent::Retrying { .. } => "retrying",
            };
            self.events.lock().unwrap().push(kind);
            Ok(())
        }
    }

    /// Records the sequence of operations so tests can assert ordering.
    #[derive(Clone, Default)]
    struct OperationLog {
        ops: Arc<Mutex<Vec<String>>>,
    }

    impl OperationLog {
        fn push(&self, op: impl Into<String>) {
            self.ops.lock().unwrap().push(op.into());
        }

        fn snapshot(&self) -> Vec<String> {
            self.ops.lock().unwrap().clone()
        }
    }

    /// Attachment store that records each delete in a shared operation log.
    #[derive(Clone)]
    struct LoggingDeleteStore {
        log: OperationLog,
        deleted: Arc<Mutex<Vec<BlobRef>>>,
    }

    impl LoggingDeleteStore {
        fn new(log: OperationLog) -> Self {
            Self {
                log,
                deleted: Arc::new(Mutex::new(vec![])),
            }
        }
    }

    impl AttachmentStore for LoggingDeleteStore {
        async fn put(&self, _: AttachmentReader) -> Result<PutResult, AttachmentStoreError> {
            unimplemented!()
        }

        async fn get(&self, _: &BlobRef) -> Result<AttachmentReader, AttachmentStoreError> {
            unimplemented!()
        }

        async fn delete(&self, blob: &BlobRef) -> Result<(), AttachmentStoreError> {
            self.log.push(format!("delete:{}", blob.key));
            self.deleted.lock().unwrap().push(blob.clone());
            Ok(())
        }
    }

    /// Repository that records `set_attachments` calls in a shared operation log.
    #[derive(Clone)]
    struct LoggingRepository {
        log: OperationLog,
    }

    impl LoggingRepository {
        fn new(log: OperationLog) -> Self {
            Self { log }
        }
    }

    impl EmailRepository for LoggingRepository {
        async fn save(&self, _: EmailId, _: &Envelope) -> Result<SaveResult, EmailRepositoryError> {
            unimplemented!()
        }

        async fn list_all_attachment_blobs(
            &self,
        ) -> Result<Vec<catapulte_domain::entity::attachment::BlobRef>, EmailRepositoryError>
        {
            unimplemented!()
        }

        async fn list_emails(
            &self,
            _: ListEmailsParams,
        ) -> Result<Vec<EmailRecord>, EmailRepositoryError> {
            unimplemented!()
        }

        async fn set_attachments(
            &self,
            id: EmailId,
            _: &[AttachmentRef],
        ) -> Result<(), EmailRepositoryError> {
            self.log.push(format!("set_attachments:{}", id.as_uuid()));
            Ok(())
        }

        async fn delete(&self, _: EmailId) -> Result<(), EmailRepositoryError> {
            unimplemented!()
        }
    }

    /// Noop repository used by tests that don't care about repository calls.
    #[derive(Clone)]
    struct NoopRepository;

    impl EmailRepository for NoopRepository {
        async fn save(&self, _: EmailId, _: &Envelope) -> Result<SaveResult, EmailRepositoryError> {
            unimplemented!()
        }

        async fn list_all_attachment_blobs(
            &self,
        ) -> Result<Vec<catapulte_domain::entity::attachment::BlobRef>, EmailRepositoryError>
        {
            unimplemented!()
        }

        async fn list_emails(
            &self,
            _: ListEmailsParams,
        ) -> Result<Vec<EmailRecord>, EmailRepositoryError> {
            unimplemented!()
        }

        async fn set_attachments(
            &self,
            _: EmailId,
            _: &[AttachmentRef],
        ) -> Result<(), EmailRepositoryError> {
            Ok(())
        }

        async fn delete(&self, _: EmailId) -> Result<(), EmailRepositoryError> {
            unimplemented!()
        }
    }

    /// Simple capturing store used by tests that only need to observe deletes.
    #[derive(Clone, Default)]
    struct CapturingDeleteStore {
        deleted: Arc<Mutex<Vec<BlobRef>>>,
    }

    impl AttachmentStore for CapturingDeleteStore {
        async fn put(&self, _: AttachmentReader) -> Result<PutResult, AttachmentStoreError> {
            unimplemented!()
        }

        async fn get(&self, _: &BlobRef) -> Result<AttachmentReader, AttachmentStoreError> {
            unimplemented!()
        }

        async fn delete(&self, blob: &BlobRef) -> Result<(), AttachmentStoreError> {
            self.deleted.lock().unwrap().push(blob.clone());
            Ok(())
        }
    }

    #[derive(Clone)]
    struct TestState<Q: Clone + Send + Sync + 'static> {
        queue: Q,
        publisher: RecordingPublisher,
        store: CapturingDeleteStore,
        repository: NoopRepository,
    }

    impl<Q: EmailQueue + Clone + Send + Sync + 'static> WorkerState for TestState<Q> {
        fn process_queued_email(&self) -> &impl ProcessQueuedEmailUseCase {
            &OkProcessor
        }

        fn email_queue(&self) -> &impl EmailQueue {
            &self.queue
        }

        fn event_publisher(&self) -> &impl EventPublisher {
            &self.publisher
        }

        fn attachment_store(&self) -> &impl AttachmentStore {
            &self.store
        }

        fn email_repository(&self) -> &impl EmailRepository {
            &self.repository
        }
    }

    #[derive(Clone)]
    struct NoopQueue;

    impl EmailQueue for NoopQueue {
        async fn enqueue(&self, _: EmailId, _: &Envelope) -> Result<(), EmailQueueError> {
            Ok(())
        }

        async fn dequeue(&self) -> Result<DequeuedEmail, EmailQueueError> {
            std::future::pending().await
        }

        async fn ack(&self, _: AckToken) -> Result<(), EmailQueueError> {
            Ok(())
        }

        async fn nack(&self, _: AckToken, _: Duration) -> Result<(), EmailQueueError> {
            Ok(())
        }
    }

    #[derive(Clone)]
    struct NoopPublisher;

    impl EventPublisher for NoopPublisher {
        async fn publish(&self, _: &LifecycleEvent) -> Result<(), EventPublisherError> {
            Ok(())
        }
    }

    #[derive(Clone)]
    struct NoopStore;

    impl AttachmentStore for NoopStore {
        async fn put(&self, _: AttachmentReader) -> Result<PutResult, AttachmentStoreError> {
            unimplemented!()
        }

        async fn get(&self, _: &BlobRef) -> Result<AttachmentReader, AttachmentStoreError> {
            unimplemented!()
        }

        async fn delete(&self, _: &BlobRef) -> Result<(), AttachmentStoreError> {
            Ok(())
        }
    }

    #[derive(Clone)]
    struct TestStateNoop;

    impl WorkerState for TestStateNoop {
        fn process_queued_email(&self) -> &impl ProcessQueuedEmailUseCase {
            &OkProcessor
        }

        fn email_queue(&self) -> &impl EmailQueue {
            &NoopQueue
        }

        fn event_publisher(&self) -> &impl EventPublisher {
            &NoopPublisher
        }

        fn attachment_store(&self) -> &impl AttachmentStore {
            &NoopStore
        }

        fn email_repository(&self) -> &impl EmailRepository {
            &NoopRepository
        }
    }

    /// State that wires logging store + logging repository together.
    #[derive(Clone)]
    struct OrderCheckState {
        log: OperationLog,
        store: LoggingDeleteStore,
        repository: LoggingRepository,
    }

    impl OrderCheckState {
        fn new() -> Self {
            let log = OperationLog::default();
            Self {
                store: LoggingDeleteStore::new(log.clone()),
                repository: LoggingRepository::new(log.clone()),
                log,
            }
        }
    }

    impl WorkerState for OrderCheckState {
        fn process_queued_email(&self) -> &impl ProcessQueuedEmailUseCase {
            &OkProcessor
        }

        fn email_queue(&self) -> &impl EmailQueue {
            &NoopQueue
        }

        fn event_publisher(&self) -> &impl EventPublisher {
            &NoopPublisher
        }

        fn attachment_store(&self) -> &impl AttachmentStore {
            &self.store
        }

        fn email_repository(&self) -> &impl EmailRepository {
            &self.repository
        }
    }

    #[tokio::test]
    async fn run_stops_when_token_is_cancelled() {
        use tokio_util::sync::CancellationToken;

        let cancel = CancellationToken::new();
        let worker = Worker {};
        let cancel_clone = cancel.clone();

        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            cancel_clone.cancel();
        });

        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            worker.run(TestStateNoop, cancel),
        )
        .await
        .expect("Worker::run must return after cancellation");
    }

    #[tokio::test]
    async fn ack_failure_after_successful_send_suppresses_sent_event() {
        let publisher = RecordingPublisher::default();
        let state = TestState {
            queue: FailingAckQueue,
            publisher: publisher.clone(),
            store: CapturingDeleteStore::default(),
            repository: NoopRepository,
        };
        let id = EmailId::default();
        let token = AckToken::new(vec![0u8; 8]);

        process_one(
            &state,
            id,
            sample_envelope(),
            1,
            token,
            TraceCarrier::default(),
        )
        .await;

        let events = publisher.events.lock().unwrap();
        assert!(
            events.contains(&"sending"),
            "sending event should be published before the attempt"
        );
        assert!(
            !events.contains(&"sent"),
            "sent event must not be published when ack fails"
        );
    }

    #[tokio::test]
    async fn sent_email_blobs_are_deleted() {
        let store = CapturingDeleteStore::default();
        let state = TestState {
            queue: NoopQueue,
            publisher: RecordingPublisher::default(),
            store: store.clone(),
            repository: NoopRepository,
        };

        let blob = BlobRef {
            backend: "fs".into(),
            key: "deadbeef01".into(),
        };
        let mut envelope = sample_envelope();
        envelope.attachments = vec![AttachmentRef {
            filename: "file.pdf".into(),
            content_type: "application/pdf".into(),
            size_bytes: 1024,
            blob: blob.clone(),
        }];

        let id = EmailId::default();
        let token = AckToken::new(vec![0u8; 8]);

        process_one(&state, id, envelope, 1, token, TraceCarrier::default()).await;

        let deleted = store.deleted.lock().unwrap();
        assert!(
            deleted.contains(&blob),
            "expected blob to be deleted after successful send, got: {deleted:?}"
        );
    }

    #[tokio::test]
    async fn set_attachments_called_before_delete() {
        let state = OrderCheckState::new();
        let id = EmailId::default();

        let blob = BlobRef {
            backend: "fs".into(),
            key: "blobkey42".into(),
        };
        let mut envelope = sample_envelope();
        envelope.attachments = vec![AttachmentRef {
            filename: "file.pdf".into(),
            content_type: "application/pdf".into(),
            size_bytes: 512,
            blob: blob.clone(),
        }];

        let token = AckToken::new(vec![0u8; 8]);
        process_one(&state, id, envelope, 1, token, TraceCarrier::default()).await;

        let ops = state.log.snapshot();
        let set_pos = ops
            .iter()
            .position(|op| op.starts_with("set_attachments:"))
            .expect("set_attachments must be called");
        let delete_pos = ops
            .iter()
            .position(|op| op.starts_with("delete:"))
            .expect("delete must be called");
        assert!(
            set_pos < delete_pos,
            "set_attachments must be called before delete, got ops: {ops:?}"
        );
    }

    struct NoMatchingRouteProcessor;

    impl ProcessQueuedEmailUseCase for NoMatchingRouteProcessor {
        async fn execute(&self, _: Envelope) -> Result<SenderName, ProcessQueuedEmailError> {
            Err(ProcessQueuedEmailError::Send(
                catapulte_domain::port::email_sender::SendError::NoMatchingRoute {
                    sender_domain: "example.com".to_owned(),
                },
            ))
        }
    }

    #[derive(Clone, Default)]
    struct TrackingQueue {
        acked: Arc<Mutex<u32>>,
        nacked: Arc<Mutex<u32>>,
    }

    impl EmailQueue for TrackingQueue {
        async fn enqueue(&self, _: EmailId, _: &Envelope) -> Result<(), EmailQueueError> {
            Ok(())
        }

        async fn dequeue(&self) -> Result<DequeuedEmail, EmailQueueError> {
            std::future::pending().await
        }

        async fn ack(&self, _: AckToken) -> Result<(), EmailQueueError> {
            *self.acked.lock().unwrap() += 1;
            Ok(())
        }

        async fn nack(&self, _: AckToken, _: Duration) -> Result<(), EmailQueueError> {
            *self.nacked.lock().unwrap() += 1;
            Ok(())
        }
    }

    #[derive(Clone)]
    struct NoMatchingRouteState {
        queue: TrackingQueue,
        publisher: RecordingPublisher,
    }

    impl WorkerState for NoMatchingRouteState {
        fn process_queued_email(&self) -> &impl ProcessQueuedEmailUseCase {
            &NoMatchingRouteProcessor
        }

        fn email_queue(&self) -> &impl EmailQueue {
            &self.queue
        }

        fn event_publisher(&self) -> &impl EventPublisher {
            &self.publisher
        }

        fn attachment_store(&self) -> &impl AttachmentStore {
            &NoopStore
        }

        fn email_repository(&self) -> &impl EmailRepository {
            &NoopRepository
        }
    }

    #[tokio::test]
    async fn non_transient_send_error_fails_immediately_without_retry() {
        let queue = TrackingQueue::default();
        let publisher = RecordingPublisher::default();
        let state = NoMatchingRouteState {
            queue: queue.clone(),
            publisher: publisher.clone(),
        };

        let id = EmailId::default();
        let token = AckToken::new(vec![0u8; 8]);

        // attempt = 1, well below MAX_ATTEMPTS=3, so only the non-transient check drives failure
        process_one(
            &state,
            id,
            sample_envelope(),
            1,
            token,
            TraceCarrier::default(),
        )
        .await;

        assert_eq!(
            *queue.acked.lock().unwrap(),
            1,
            "non-transient error must ack (not nack) even on first attempt"
        );
        assert_eq!(
            *queue.nacked.lock().unwrap(),
            0,
            "non-transient error must not nack"
        );

        let events = publisher.events.lock().unwrap();
        assert!(
            events.contains(&"failed"),
            "expected 'failed' lifecycle event, got: {events:?}"
        );
        assert!(
            !events.contains(&"retrying"),
            "must not emit 'retrying' for non-transient error"
        );
    }
}
