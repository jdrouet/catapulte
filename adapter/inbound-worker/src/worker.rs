use catapulte_domain::entity::lifecycle_event::LifecycleEvent;
use catapulte_domain::port::attachment_store::AttachmentStore;
use catapulte_domain::port::clock::{Clock, SystemClock};
use catapulte_domain::port::email_queue::{AckToken, EmailQueue};
use catapulte_domain::port::email_repository::{DeliveryStart, EmailCancellation, EmailRepository};
use catapulte_domain::port::event_publisher::EventPublisher;
use catapulte_domain::use_case::process_queued_email::ProcessQueuedEmailUseCase;
use tracing::Instrument as _;

const MAX_ATTEMPTS: u32 = 3;

pub trait WorkerState: Clone + Send + Sync + 'static {
    fn process_queued_email(&self) -> &impl ProcessQueuedEmailUseCase;
    fn email_queue(&self) -> &impl EmailQueue;
    fn event_publisher(&self) -> &impl EventPublisher;
    fn attachment_store(&self) -> &impl AttachmentStore;
    fn email_repository(&self) -> &(impl EmailRepository + EmailCancellation);
}

pub struct WorkerConfig {
    concurrency: usize,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self { concurrency: 1 }
    }
}

impl WorkerConfig {
    /// # Errors
    ///
    /// Returns an error if `CATAPULTE_WORKER_CONCURRENCY` is set but cannot be
    /// parsed as a positive integer.
    pub fn from_env(prefix: &str) -> anyhow::Result<Self> {
        use anyhow::Context as _;

        let concurrency_key = format!("{prefix}_CONCURRENCY");
        let concurrency: usize = match std::env::var(&concurrency_key) {
            Err(std::env::VarError::NotPresent) => 1,
            Err(e) => {
                return Err(anyhow::Error::new(e).context(format!("reading {concurrency_key}")));
            }
            Ok(v) => v
                .parse()
                .with_context(|| format!("invalid {concurrency_key}: {v:?}"))?,
        };
        if concurrency == 0 {
            anyhow::bail!("{concurrency_key} must be at least 1 (got 0)");
        }
        Ok(Self { concurrency })
    }

    #[must_use]
    pub fn build(self) -> Worker {
        Worker {
            concurrency: self.concurrency,
            clock: std::sync::Arc::new(SystemClock),
        }
    }
}

pub struct Worker<C: Clock = SystemClock> {
    concurrency: usize,
    clock: std::sync::Arc<C>,
}

/// A wait longer than this means the firing source and this worker disagree about the time.
const SKEW_WARN_AFTER: std::time::Duration = std::time::Duration::from_secs(30);

fn backoff(attempt: u32) -> std::time::Duration {
    let secs = (30u64 * (1u64 << attempt.saturating_sub(1))).min(3600);
    std::time::Duration::from_secs(secs)
}

impl Worker {
    /// Replaces the wall clock used to decide whether a scheduled email is due.
    #[must_use]
    pub fn with_clock<C: Clock>(self, clock: C) -> Worker<C> {
        Worker {
            concurrency: self.concurrency,
            clock: std::sync::Arc::new(clock),
        }
    }
}

impl<C: Clock> Worker<C> {
    /// # Panics
    ///
    /// Never panics in practice: the `expect` on the semaphore acquire is
    /// unreachable because the semaphore is never closed.
    pub async fn run<S: WorkerState>(self, state: S, cancel: tokio_util::sync::CancellationToken) {
        tracing::info!(concurrency = self.concurrency, "worker started");

        let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(self.concurrency));
        let mut tasks: tokio::task::JoinSet<()> = tokio::task::JoinSet::new();

        loop {
            let permit = tokio::select! {
                biased;
                () = cancel.cancelled() => break,
                p = sem.clone().acquire_owned() => p.expect("semaphore is never closed"),
            };

            let dequeued = tokio::select! {
                biased;
                () = cancel.cancelled() => { drop(permit); break }
                result = state.email_queue().dequeue() => result,
            };

            match dequeued {
                Ok(dequeued) => {
                    // Clone the state for the spawned task. WorkerState impls
                    // hold Arc-wrapped internals so clones are cheap.
                    let st = state.clone();
                    let clock = std::sync::Arc::clone(&self.clock);
                    let task_cancel = cancel.clone();
                    tasks.spawn(async move {
                        let _permit = permit; // holds the slot until the task finishes
                        process_one(
                            &st,
                            clock.as_ref(),
                            &task_cancel,
                            dequeued.id,
                            dequeued.envelope,
                            dequeued.attempt,
                            dequeued.token,
                            dequeued.trace,
                        )
                        .await;
                    });
                    // Reap finished tasks so the JoinSet does not grow unbounded.
                    while tasks.try_join_next().is_some() {}
                }
                Err(e) => {
                    tracing::error!(error = %e, "failed to dequeue");
                    drop(permit);
                    tokio::select! {
                        biased;
                        () = cancel.cancelled() => break,
                        () = tokio::time::sleep(std::time::Duration::from_secs(1)) => {}
                    }
                }
            }
        }

        // Graceful drain: let in-flight tasks finish (bounded externally by the
        // binary's 30 s shutdown budget).
        while tasks.join_next().await.is_some() {}
        tracing::info!("worker stopped");
    }
}

/// Holds the claim until `send_at_ms` has passed on `clock`, renewing it every
/// `renew_interval` and never giving the message back. Returns `false` when the claim could
/// not be renewed or the worker is shutting down; the queue then redelivers the message once
/// the lease lapses.
async fn wait_until_due<S: WorkerState>(
    state: &S,
    clock: &impl Clock,
    cancel: &tokio_util::sync::CancellationToken,
    id: catapulte_domain::entity::email::EmailId,
    send_at_ms: i64,
    token: &AckToken,
) -> bool {
    let queue = state.email_queue();
    let interval = queue.renew_interval();
    let started = tokio::time::Instant::now();
    let mut warned = false;
    loop {
        let now = clock.now_ms();
        if now >= send_at_ms {
            return true;
        }
        if let Err(e) = queue.in_progress(token).await {
            tracing::error!(
                error = %e,
                email_id = %id.as_uuid(),
                "failed to renew the claim while waiting for the send time"
            );
            return false;
        }
        let remaining = std::time::Duration::from_millis(send_at_ms.abs_diff(now));
        tokio::select! {
            biased;
            () = cancel.cancelled() => return false,
            () = tokio::time::sleep(interval.min(remaining)) => {}
        }
        if !warned && started.elapsed() > SKEW_WARN_AFTER {
            warned = true;
            tracing::warn!(
                email_id = %id.as_uuid(),
                send_at_ms,
                "scheduled email still not due after 30 s, check the clock skew"
            );
        }
    }
}

#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
async fn process_one<S: WorkerState>(
    state: &S,
    clock: &impl Clock,
    cancel: &tokio_util::sync::CancellationToken,
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
        let context = envelope.event_context();
        if let Some(ref cid) = correlation_id {
            tracing::Span::current().record("correlation_id", cid.as_str());
        }

        if let Some(send_at_ms) = envelope.send_at_ms {
            if !wait_until_due(state, clock, cancel, id, send_at_ms, &token).await {
                return;
            }
            match state.email_repository().start_delivery(id).await {
                Ok(DeliveryStart::Proceed) => {}
                Ok(DeliveryStart::Cancelled) => {
                    if let Err(e) = state.email_queue().ack(token).await {
                        tracing::error!(error = %e, email_id = %id.as_uuid(), "failed to ack cancelled email");
                    }
                    return;
                }
                Err(e) => {
                    tracing::error!(error = %e, email_id = %id.as_uuid(), "failed to record the delivery start");
                    if let Err(nack_err) = state.email_queue().nack(token, backoff(attempt)).await {
                        tracing::error!(error = %nack_err, "failed to nack after a fence error");
                    }
                    return;
                }
            }
        }

        if let Err(e) = state
            .event_publisher()
            .publish(&LifecycleEvent::Sending {
                id,
                attempt,
                context: context.clone(),
            })
            .await
        {
            tracing::error!(error = %e, "failed to publish sending event");
        }

        let attachments_for_cleanup = envelope.attachments.clone();

        match state.process_queued_email().execute(id, envelope).await {
            Ok(processed) => {
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
                        sender_name: processed.sender_name,
                        context: context.clone(),
                        smtp_response: processed.smtp_response,
                        message_id: processed.message_id,
                    })
                    .await
                {
                    tracing::error!(error = %e, "failed to publish sent event");
                }
                if !processed.suppressed.is_empty()
                    && let Err(e) = state
                        .event_publisher()
                        .publish(&LifecycleEvent::Suppressed {
                            id,
                            addresses: processed.suppressed,
                            context: context.clone(),
                        })
                        .await
                {
                    tracing::error!(error = %e, "failed to publish suppressed event");
                }
            }
            Err(e) => {
                tracing::error!(
                    error = ?e,
                    email_id = %id.as_uuid(),
                    attempt,
                    "failed to process email"
                );
                let reason = e.to_string().replace('\0', "");
                let error_class = e.error_class();
                let sender_name = e.sender_name().cloned();
                let is_terminal = e.is_permanent() || attempt >= MAX_ATTEMPTS;
                let event = if is_terminal {
                    if let Err(ack_err) = state.email_queue().ack(token).await {
                        tracing::error!(error = %ack_err, "failed to ack permanently failed email");
                        return;
                    }
                    LifecycleEvent::Failed {
                        id,
                        attempt,
                        reason,
                        error_class,
                        sender_name,
                        context: context.clone(),
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
                        error_class,
                        sender_name,
                        context: context.clone(),
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
        AckToken, DequeuedEmail, EmailQueue, EmailQueueError, EnqueueOutcome, TraceCarrier,
    };
    use catapulte_domain::port::email_repository::{
        CancelOutcome, DeliveryStart, EmailCancellation, EmailRecord, EmailRepository,
        EmailRepositoryError, EnqueueState, ListEmailsParams, SaveResult,
    };
    use catapulte_domain::port::event_publisher::{EventPublisher, EventPublisherError};
    use catapulte_domain::use_case::process_queued_email::{
        ProcessQueuedEmailError, ProcessQueuedEmailUseCase, ProcessedEmail,
    };

    use super::{Worker, WorkerConfig, WorkerState, process_one};
    use catapulte_domain::port::clock::{Clock, SystemClock};

    fn processed(sender: &str) -> ProcessedEmail {
        ProcessedEmail {
            sender_name: SenderName::new(sender),
            smtp_response: Some("250 2.0.0 Ok: queued".to_owned()),
            message_id: "<m@example.com>".to_owned(),
            suppressed: vec![],
        }
    }

    fn sample_envelope() -> Envelope {
        Envelope::new(
            "sender@example.com",
            vec![(RecipientKind::To, "to@example.com".to_owned())],
            BodySource::Plain(Plain::try_new(Some("hi".to_owned()), None).unwrap()),
        )
    }

    struct OkProcessor;

    impl ProcessQueuedEmailUseCase for OkProcessor {
        async fn execute(
            &self,
            _: EmailId,
            _: Envelope,
        ) -> Result<ProcessedEmail, ProcessQueuedEmailError> {
            Ok(processed("sender"))
        }
    }

    #[derive(Clone)]
    struct FailingAckQueue;

    impl EmailQueue for FailingAckQueue {
        async fn in_progress(&self, _token: &AckToken) -> Result<(), EmailQueueError> {
            Ok(())
        }

        fn renew_interval(&self) -> Duration {
            Duration::from_secs(1)
        }

        async fn enqueue(
            &self,
            _: EmailId,
            _: &Envelope,
        ) -> Result<EnqueueOutcome, EmailQueueError> {
            Ok(EnqueueOutcome::Inserted)
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
            self.events.lock().unwrap().push(event.event_type());
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

        async fn set_enqueue_state(
            &self,
            _: EmailId,
            _: &str,
            _: EnqueueState,
        ) -> Result<bool, EmailRepositoryError> {
            Ok(true)
        }

        async fn delete(&self, _: EmailId) -> Result<(), EmailRepositoryError> {
            unimplemented!()
        }
    }

    impl EmailCancellation for LoggingRepository {
        async fn cancel(
            &self,
            _: EmailId,
            _: Option<&str>,
        ) -> Result<CancelOutcome, EmailRepositoryError> {
            unimplemented!()
        }

        async fn start_delivery(&self, _: EmailId) -> Result<DeliveryStart, EmailRepositoryError> {
            Ok(DeliveryStart::Proceed)
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

        async fn set_enqueue_state(
            &self,
            _: EmailId,
            _: &str,
            _: EnqueueState,
        ) -> Result<bool, EmailRepositoryError> {
            Ok(true)
        }

        async fn delete(&self, _: EmailId) -> Result<(), EmailRepositoryError> {
            unimplemented!()
        }
    }

    impl EmailCancellation for NoopRepository {
        async fn cancel(
            &self,
            _: EmailId,
            _: Option<&str>,
        ) -> Result<CancelOutcome, EmailRepositoryError> {
            unimplemented!()
        }

        async fn start_delivery(&self, _: EmailId) -> Result<DeliveryStart, EmailRepositoryError> {
            Ok(DeliveryStart::Proceed)
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

        fn email_repository(&self) -> &(impl EmailRepository + EmailCancellation) {
            &self.repository
        }
    }

    #[derive(Clone)]
    struct NoopQueue;

    impl EmailQueue for NoopQueue {
        async fn in_progress(&self, _token: &AckToken) -> Result<(), EmailQueueError> {
            Ok(())
        }

        fn renew_interval(&self) -> Duration {
            Duration::from_secs(1)
        }

        async fn enqueue(
            &self,
            _: EmailId,
            _: &Envelope,
        ) -> Result<EnqueueOutcome, EmailQueueError> {
            Ok(EnqueueOutcome::Inserted)
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

        fn email_repository(&self) -> &(impl EmailRepository + EmailCancellation) {
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

        fn email_repository(&self) -> &(impl EmailRepository + EmailCancellation) {
            &self.repository
        }
    }

    #[tokio::test]
    async fn run_stops_when_token_is_cancelled() {
        use tokio_util::sync::CancellationToken;

        let cancel = CancellationToken::new();
        let worker = WorkerConfig::default().build().with_clock(SystemClock);
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
            &SystemClock,
            &tokio_util::sync::CancellationToken::new(),
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
            !events.contains(&"delivery.succeeded"),
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
            content_id: None,
            filename: "file.pdf".into(),
            content_type: "application/pdf".into(),
            size_bytes: 1024,
            blob: blob.clone(),
        }];

        let id = EmailId::default();
        let token = AckToken::new(vec![0u8; 8]);

        process_one(
            &state,
            &SystemClock,
            &tokio_util::sync::CancellationToken::new(),
            id,
            envelope,
            1,
            token,
            TraceCarrier::default(),
        )
        .await;

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
            content_id: None,
            filename: "file.pdf".into(),
            content_type: "application/pdf".into(),
            size_bytes: 512,
            blob: blob.clone(),
        }];

        let token = AckToken::new(vec![0u8; 8]);
        process_one(
            &state,
            &SystemClock,
            &tokio_util::sync::CancellationToken::new(),
            id,
            envelope,
            1,
            token,
            TraceCarrier::default(),
        )
        .await;

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
        async fn execute(
            &self,
            _: EmailId,
            _: Envelope,
        ) -> Result<ProcessedEmail, ProcessQueuedEmailError> {
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
        async fn in_progress(&self, _token: &AckToken) -> Result<(), EmailQueueError> {
            Ok(())
        }

        fn renew_interval(&self) -> Duration {
            Duration::from_secs(1)
        }

        async fn enqueue(
            &self,
            _: EmailId,
            _: &Envelope,
        ) -> Result<EnqueueOutcome, EmailQueueError> {
            Ok(EnqueueOutcome::Inserted)
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

        fn email_repository(&self) -> &(impl EmailRepository + EmailCancellation) {
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
            &SystemClock,
            &tokio_util::sync::CancellationToken::new(),
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
            events.contains(&"delivery.failed"),
            "expected 'delivery.failed' lifecycle event, got: {events:?}"
        );
        assert!(
            !events.contains(&"retrying"),
            "must not emit 'retrying' for non-transient error"
        );
    }

    #[derive(Clone, Default)]
    struct CapturingEventPublisher {
        events: Arc<Mutex<Vec<LifecycleEvent>>>,
    }

    impl EventPublisher for CapturingEventPublisher {
        async fn publish(&self, event: &LifecycleEvent) -> Result<(), EventPublisherError> {
            self.events.lock().unwrap().push(event.clone());
            Ok(())
        }
    }

    #[derive(Clone)]
    struct CapturingEventState {
        queue: TrackingQueue,
        publisher: CapturingEventPublisher,
    }

    impl WorkerState for CapturingEventState {
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

        fn email_repository(&self) -> &(impl EmailRepository + EmailCancellation) {
            &NoopRepository
        }
    }

    #[derive(Clone)]
    struct OkCapturingState {
        queue: TrackingQueue,
        publisher: CapturingEventPublisher,
    }

    impl WorkerState for OkCapturingState {
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
            &NoopStore
        }

        fn email_repository(&self) -> &(impl EmailRepository + EmailCancellation) {
            &NoopRepository
        }
    }

    #[tokio::test]
    async fn sent_event_carries_smtp_response_and_message_id() {
        let publisher = CapturingEventPublisher::default();
        let state = OkCapturingState {
            queue: TrackingQueue::default(),
            publisher: publisher.clone(),
        };

        process_one(
            &state,
            &SystemClock,
            &tokio_util::sync::CancellationToken::new(),
            EmailId::default(),
            sample_envelope(),
            1,
            AckToken::new(vec![0u8; 8]),
            TraceCarrier::default(),
        )
        .await;

        let events = publisher.events.lock().unwrap();
        let sent = events
            .iter()
            .find(|e| matches!(e, LifecycleEvent::Sent { .. }))
            .expect("must have a Sent event");
        assert!(matches!(
            sent,
            LifecycleEvent::Sent { smtp_response, message_id, .. }
                if smtp_response.as_deref() == Some("250 2.0.0 Ok: queued")
                    && message_id == "<m@example.com>"
        ));
    }

    #[tokio::test]
    async fn no_matching_route_emits_failed_event_with_routing_error_class() {
        use catapulte_domain::entity::error_class::ErrorClass;

        let queue = TrackingQueue::default();
        let publisher = CapturingEventPublisher::default();
        let state = CapturingEventState {
            queue,
            publisher: publisher.clone(),
        };

        let id = EmailId::default();
        let token = AckToken::new(vec![0u8; 8]);

        process_one(
            &state,
            &SystemClock,
            &tokio_util::sync::CancellationToken::new(),
            id,
            sample_envelope(),
            1,
            token,
            TraceCarrier::default(),
        )
        .await;

        let events = publisher.events.lock().unwrap();
        let failed = events
            .iter()
            .find(|e| matches!(e, LifecycleEvent::Failed { .. }))
            .expect("must have a Failed event");
        assert!(
            matches!(failed, LifecycleEvent::Failed { error_class, .. } if *error_class == ErrorClass::Routing),
            "NoMatchingRoute must produce Routing error class, got: {failed:?}"
        );
    }

    #[derive(Clone)]
    struct ScriptedProcessor {
        all_suppressed: bool,
        over_budget: bool,
    }

    impl ProcessQueuedEmailUseCase for ScriptedProcessor {
        async fn execute(
            &self,
            _: EmailId,
            _: Envelope,
        ) -> Result<ProcessedEmail, ProcessQueuedEmailError> {
            if self.all_suppressed {
                return Err(ProcessQueuedEmailError::AllRecipientsSuppressed);
            }
            if self.over_budget {
                return Err(ProcessQueuedEmailError::Interpolate(
                    catapulte_domain::port::template_interpolator::InterpolateError::BudgetExceeded,
                ));
            }
            Ok(ProcessedEmail {
                suppressed: vec!["b@x.com".to_owned()],
                ..processed("sender")
            })
        }
    }

    #[derive(Clone)]
    struct ScriptedState {
        processor: ScriptedProcessor,
        queue: TrackingQueue,
        publisher: CapturingEventPublisher,
    }

    impl WorkerState for ScriptedState {
        fn process_queued_email(&self) -> &impl ProcessQueuedEmailUseCase {
            &self.processor
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

        fn email_repository(&self) -> &(impl EmailRepository + EmailCancellation) {
            &NoopRepository
        }
    }

    async fn run_scripted(all_suppressed: bool) -> (TrackingQueue, Vec<LifecycleEvent>) {
        let state = ScriptedState {
            processor: ScriptedProcessor {
                all_suppressed,
                over_budget: false,
            },
            queue: TrackingQueue::default(),
            publisher: CapturingEventPublisher::default(),
        };
        process_one(
            &state,
            &SystemClock,
            &tokio_util::sync::CancellationToken::new(),
            EmailId::default(),
            sample_envelope(),
            1,
            AckToken::new(vec![0u8; 8]),
            TraceCarrier::default(),
        )
        .await;
        let events = state.publisher.events.lock().unwrap().clone();
        (state.queue, events)
    }

    #[tokio::test]
    async fn worker_events_carry_the_envelope_tags_and_metadata() {
        let state = ScriptedState {
            processor: ScriptedProcessor {
                all_suppressed: false,
                over_budget: false,
            },
            queue: TrackingQueue::default(),
            publisher: CapturingEventPublisher::default(),
        };
        let mut envelope = sample_envelope();
        envelope.tags = vec!["welcome".to_owned()];
        envelope
            .metadata
            .insert("order".to_owned(), "42".to_owned());
        process_one(
            &state,
            &SystemClock,
            &tokio_util::sync::CancellationToken::new(),
            EmailId::default(),
            envelope,
            1,
            AckToken::new(vec![0u8; 8]),
            TraceCarrier::default(),
        )
        .await;

        let events = state.publisher.events.lock().unwrap();
        assert_eq!(events.len(), 3);
        for event in events.iter() {
            let payload = event.payload();
            assert_eq!(
                payload["tags"],
                serde_json::json!(["welcome"]),
                "{}",
                event.event_type()
            );
            assert_eq!(payload["metadata"], serde_json::json!({ "order": "42" }));
        }
    }

    #[tokio::test]
    async fn all_recipients_suppressed_acks_and_fails_with_the_suppressed_class() {
        use catapulte_domain::entity::error_class::ErrorClass;

        let (queue, events) = run_scripted(true).await;

        assert_eq!(*queue.acked.lock().unwrap(), 1);
        assert_eq!(*queue.nacked.lock().unwrap(), 0);
        assert!(events.iter().any(|e| matches!(
            e,
            LifecycleEvent::Failed { error_class, .. } if *error_class == ErrorClass::Suppressed
        )));
    }

    #[tokio::test]
    async fn an_exhausted_render_budget_fails_the_email_without_a_retry() {
        use catapulte_domain::entity::error_class::ErrorClass;

        let state = ScriptedState {
            processor: ScriptedProcessor {
                all_suppressed: false,
                over_budget: true,
            },
            queue: TrackingQueue::default(),
            publisher: CapturingEventPublisher::default(),
        };
        process_one(
            &state,
            &SystemClock,
            &tokio_util::sync::CancellationToken::new(),
            EmailId::default(),
            sample_envelope(),
            1,
            AckToken::new(vec![0u8; 8]),
            TraceCarrier::default(),
        )
        .await;

        assert_eq!(*state.queue.acked.lock().unwrap(), 1);
        assert_eq!(*state.queue.nacked.lock().unwrap(), 0);
        let events = state.publisher.events.lock().unwrap();
        assert!(events.iter().any(|e| matches!(
            e,
            LifecycleEvent::Failed { error_class, .. } if *error_class == ErrorClass::TemplateInterpolate
        )));
        assert!(!events.iter().any(|e| e.event_type() == "retrying"));
    }

    #[tokio::test]
    async fn partial_suppression_sends_then_announces_the_dropped_recipients() {
        let (_, events) = run_scripted(false).await;

        let types: Vec<_> = events.iter().map(LifecycleEvent::event_type).collect();
        assert_eq!(types, ["sending", "delivery.succeeded", "suppressed"]);
    }

    // -------------------------------------------------------------------------
    // Helpers for Worker::run concurrency tests
    // -------------------------------------------------------------------------

    /// A queue that yields exactly `count` messages (using sequential fake IDs)
    /// and then blocks forever, so the worker loop is kept alive without
    /// consuming more slots.
    #[derive(Clone)]
    struct FiniteQueue {
        remaining: Arc<Mutex<u32>>,
        acked: Arc<Mutex<u32>>,
    }

    impl FiniteQueue {
        fn new(count: u32) -> Self {
            Self {
                remaining: Arc::new(Mutex::new(count)),
                acked: Arc::new(Mutex::new(0)),
            }
        }
    }

    impl EmailQueue for FiniteQueue {
        async fn in_progress(&self, _token: &AckToken) -> Result<(), EmailQueueError> {
            Ok(())
        }

        fn renew_interval(&self) -> Duration {
            Duration::from_secs(1)
        }

        async fn enqueue(
            &self,
            _: EmailId,
            _: &Envelope,
        ) -> Result<EnqueueOutcome, EmailQueueError> {
            Ok(EnqueueOutcome::Inserted)
        }

        async fn dequeue(&self) -> Result<DequeuedEmail, EmailQueueError> {
            {
                let mut rem = self.remaining.lock().unwrap();
                if *rem > 0 {
                    *rem -= 1;
                    let id = EmailId::default();
                    return Ok(DequeuedEmail {
                        id,
                        envelope: sample_envelope(),
                        attempt: 1,
                        token: AckToken::new(vec![0u8; 8]),
                        trace: TraceCarrier::default(),
                    });
                }
            }
            // All messages handed out — block until the future is dropped.
            std::future::pending().await
        }

        async fn ack(&self, _: AckToken) -> Result<(), EmailQueueError> {
            *self.acked.lock().unwrap() += 1;
            Ok(())
        }

        async fn nack(&self, _: AckToken, _: Duration) -> Result<(), EmailQueueError> {
            Ok(())
        }
    }

    /// Processor that tracks the instantaneous in-flight count and records the
    /// peak.  Each invocation sleeps briefly to force overlap when concurrency
    /// > 1.
    #[derive(Clone, Default)]
    struct PeakTrackingProcessor {
        in_flight: Arc<std::sync::atomic::AtomicUsize>,
        peak: Arc<Mutex<usize>>,
        processed: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl ProcessQueuedEmailUseCase for PeakTrackingProcessor {
        async fn execute(
            &self,
            _: EmailId,
            _: Envelope,
        ) -> Result<ProcessedEmail, ProcessQueuedEmailError> {
            let current = self
                .in_flight
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                + 1;
            {
                let mut peak = self.peak.lock().unwrap();
                if current > *peak {
                    *peak = current;
                }
            }
            // Sleep long enough that concurrent tasks overlap.
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            self.in_flight
                .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            self.processed
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(processed("sender"))
        }
    }

    #[derive(Clone)]
    struct PeakTrackingState {
        queue: FiniteQueue,
        processor: PeakTrackingProcessor,
    }

    impl WorkerState for PeakTrackingState {
        fn process_queued_email(&self) -> &impl ProcessQueuedEmailUseCase {
            &self.processor
        }

        fn email_queue(&self) -> &impl EmailQueue {
            &self.queue
        }

        fn event_publisher(&self) -> &impl EventPublisher {
            &NoopPublisher
        }

        fn attachment_store(&self) -> &impl AttachmentStore {
            &NoopStore
        }

        fn email_repository(&self) -> &(impl EmailRepository + EmailCancellation) {
            &NoopRepository
        }
    }

    /// Processor that parks each task on a gate semaphore (0 initial permits)
    /// until the test releases them. Tracks instantaneous in-flight count and
    /// records the peak using atomics.
    #[derive(Clone)]
    struct GateProcessor {
        in_flight: Arc<std::sync::atomic::AtomicUsize>,
        peak: Arc<std::sync::atomic::AtomicUsize>,
        processed: Arc<std::sync::atomic::AtomicUsize>,
        gate: Arc<tokio::sync::Semaphore>,
    }

    impl GateProcessor {
        fn new(gate: Arc<tokio::sync::Semaphore>) -> Self {
            Self {
                in_flight: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                peak: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                processed: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                gate,
            }
        }
    }

    impl ProcessQueuedEmailUseCase for GateProcessor {
        async fn execute(
            &self,
            _: EmailId,
            _: Envelope,
        ) -> Result<ProcessedEmail, ProcessQueuedEmailError> {
            let current = self
                .in_flight
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                + 1;
            self.peak
                .fetch_max(current, std::sync::atomic::Ordering::SeqCst);
            // Park until the test releases the gate.
            let _permit = self
                .gate
                .acquire()
                .await
                .expect("gate semaphore is never closed");
            self.in_flight
                .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            self.processed
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(processed("sender"))
        }
    }

    #[derive(Clone)]
    struct GateState {
        queue: FiniteQueue,
        processor: GateProcessor,
    }

    impl WorkerState for GateState {
        fn process_queued_email(&self) -> &impl ProcessQueuedEmailUseCase {
            &self.processor
        }

        fn email_queue(&self) -> &impl EmailQueue {
            &self.queue
        }

        fn event_publisher(&self) -> &impl EventPublisher {
            &NoopPublisher
        }

        fn attachment_store(&self) -> &impl AttachmentStore {
            &NoopStore
        }

        fn email_repository(&self) -> &(impl EmailRepository + EmailCancellation) {
            &NoopRepository
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn bounded_concurrency_peak_equals_limit() {
        use tokio_util::sync::CancellationToken;

        const MESSAGES: u32 = 5;
        const CONCURRENCY: usize = 3;

        // Gate starts with 0 permits — each processor task parks until the test
        // releases them.
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        let processor = GateProcessor::new(gate.clone());
        let queue = FiniteQueue::new(MESSAGES);
        let state = GateState {
            queue,
            processor: processor.clone(),
        };

        let cancel = CancellationToken::new();
        let run_task = tokio::spawn(
            Worker {
                concurrency: CONCURRENCY,
                clock: Arc::new(SystemClock),
            }
            .run(state, cancel.clone()),
        );

        // Poll until exactly CONCURRENCY tasks are simultaneously in-flight.
        // This is deterministic: the worker must saturate the semaphore before
        // any task can proceed past the gate.
        let reached_limit = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if processor
                    .in_flight
                    .load(std::sync::atomic::Ordering::SeqCst)
                    == CONCURRENCY
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        })
        .await;
        assert!(
            reached_limit.is_ok(),
            "worker must saturate to {CONCURRENCY} in-flight tasks within the timeout"
        );

        // At this point exactly CONCURRENCY tasks are parked on the gate.
        assert_eq!(
            processor.peak.load(std::sync::atomic::Ordering::SeqCst),
            CONCURRENCY,
            "peak must equal the configured concurrency limit"
        );

        // Release the gate so all parked tasks (and any future ones) can complete.
        gate.add_permits(usize::MAX >> 3);

        // Wait for all messages to finish processing before cancelling, so the
        // worker can dequeue and dispatch the remaining 2 messages (beyond the
        // initial CONCURRENCY burst) without racing against cancellation.
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if processor
                    .processed
                    .load(std::sync::atomic::Ordering::SeqCst)
                    >= MESSAGES as usize
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("all messages must be processed within the timeout");

        cancel.cancel();
        tokio::time::timeout(std::time::Duration::from_secs(5), run_task)
            .await
            .expect("worker task must complete within the timeout")
            .expect("worker task must not panic");

        assert_eq!(
            processor
                .processed
                .load(std::sync::atomic::Ordering::SeqCst),
            MESSAGES as usize,
            "all messages must be processed"
        );
        assert_eq!(
            processor.peak.load(std::sync::atomic::Ordering::SeqCst),
            CONCURRENCY,
            "peak must never exceed the configured concurrency limit"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn concurrency_one_is_sequential() {
        use tokio_util::sync::CancellationToken;

        const MESSAGES: u32 = 3;

        let processor = PeakTrackingProcessor::default();
        let queue = FiniteQueue::new(MESSAGES);
        let state = PeakTrackingState {
            queue: queue.clone(),
            processor: processor.clone(),
        };

        let cancel = CancellationToken::new();
        let cancel_clone = cancel.clone();
        let processed = processor.processed.clone();

        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                if processed.load(std::sync::atomic::Ordering::SeqCst) >= MESSAGES as usize {
                    cancel_clone.cancel();
                    break;
                }
            }
        });

        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            WorkerConfig::default().build().run(state, cancel),
        )
        .await
        .expect("worker must finish within the timeout");

        let peak = *processor.peak.lock().unwrap();
        assert_eq!(
            peak, 1,
            "concurrency=1 must never have more than 1 in-flight task"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn graceful_drain_completes_in_flight_task() {
        use tokio_util::sync::CancellationToken;

        // A processor that signals when it has started and then completes after
        // a short delay, recording that it finished.
        #[derive(Clone, Default)]
        struct SlowProcessor {
            started: Arc<tokio::sync::Notify>,
            finished: Arc<std::sync::atomic::AtomicBool>,
        }

        impl ProcessQueuedEmailUseCase for SlowProcessor {
            async fn execute(
                &self,
                _: EmailId,
                _: Envelope,
            ) -> Result<ProcessedEmail, ProcessQueuedEmailError> {
                self.started.notify_one();
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                self.finished
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(processed("sender"))
            }
        }

        #[derive(Clone)]
        struct DrainState {
            queue: FiniteQueue,
            processor: SlowProcessor,
        }

        impl WorkerState for DrainState {
            fn process_queued_email(&self) -> &impl ProcessQueuedEmailUseCase {
                &self.processor
            }

            fn email_queue(&self) -> &impl EmailQueue {
                &self.queue
            }

            fn event_publisher(&self) -> &impl EventPublisher {
                &NoopPublisher
            }

            fn attachment_store(&self) -> &impl AttachmentStore {
                &NoopStore
            }

            fn email_repository(&self) -> &(impl EmailRepository + EmailCancellation) {
                &NoopRepository
            }
        }

        let processor = SlowProcessor::default();
        let queue = FiniteQueue::new(1);
        let state = DrainState {
            queue,
            processor: processor.clone(),
        };

        let cancel = CancellationToken::new();
        let cancel_clone = cancel.clone();
        let started = processor.started.clone();

        // Cancel once the in-flight task has started.
        tokio::spawn(async move {
            started.notified().await;
            cancel_clone.cancel();
        });

        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            Worker {
                concurrency: 2,
                clock: Arc::new(SystemClock),
            }
            .run(state, cancel),
        )
        .await
        .expect("worker must finish within the timeout");

        assert!(
            processor.finished.load(std::sync::atomic::Ordering::SeqCst),
            "in-flight task must complete (not be abandoned) on graceful drain"
        );
    }

    struct PermanentSendProcessor;

    impl ProcessQueuedEmailUseCase for PermanentSendProcessor {
        async fn execute(
            &self,
            _: EmailId,
            _: Envelope,
        ) -> Result<ProcessedEmail, ProcessQueuedEmailError> {
            Err(ProcessQueuedEmailError::Send(
                catapulte_domain::port::email_sender::SendError::Send {
                    sender_name: SenderName::new("primary"),
                    permanent: true,
                    source: anyhow::anyhow!(
                        "smtp send failed: permanent error (550): 5.1.1 no such user"
                    ),
                },
            ))
        }
    }

    #[derive(Clone)]
    struct PermanentSendState {
        queue: TrackingQueue,
        publisher: CapturingEventPublisher,
    }

    impl WorkerState for PermanentSendState {
        fn process_queued_email(&self) -> &impl ProcessQueuedEmailUseCase {
            &PermanentSendProcessor
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

        fn email_repository(&self) -> &(impl EmailRepository + EmailCancellation) {
            &NoopRepository
        }
    }

    #[tokio::test]
    async fn permanent_send_failure_on_first_attempt_is_rejected_without_retry() {
        let queue = TrackingQueue::default();
        let publisher = CapturingEventPublisher::default();
        let state = PermanentSendState {
            queue: queue.clone(),
            publisher: publisher.clone(),
        };

        process_one(
            &state,
            &SystemClock,
            &tokio_util::sync::CancellationToken::new(),
            EmailId::default(),
            sample_envelope(),
            1,
            AckToken::new(vec![0u8; 8]),
            TraceCarrier::default(),
        )
        .await;

        assert_eq!(*queue.acked.lock().unwrap(), 1);
        assert_eq!(*queue.nacked.lock().unwrap(), 0);
        let events = publisher.events.lock().unwrap();
        let failed = events
            .iter()
            .find_map(|e| match e {
                LifecycleEvent::Failed {
                    attempt,
                    reason,
                    error_class,
                    ..
                } => Some((*attempt, reason.clone(), error_class.clone())),
                _ => None,
            })
            .expect("expected a delivery.failed event");
        assert_eq!(failed.0, 1);
        assert!(failed.1.contains("550"), "reason: {}", failed.1);
        assert_eq!(
            failed.2,
            catapulte_domain::entity::error_class::ErrorClass::Rejected
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, LifecycleEvent::Retrying { .. }))
        );
    }

    /// Wall clock that follows tokio's (paused) time, starting at `BASE_MS`.
    #[derive(Clone)]
    struct TimeClock {
        start: tokio::time::Instant,
    }

    const BASE_MS: i64 = 1_800_000_000_000;

    impl TimeClock {
        fn new() -> Self {
            Self {
                start: tokio::time::Instant::now(),
            }
        }
    }

    impl Clock for TimeClock {
        fn now_ms(&self) -> i64 {
            BASE_MS + i64::try_from(self.start.elapsed().as_millis()).unwrap()
        }
    }

    /// Returns the scripted readings in order, then repeats the last one.
    #[derive(Clone)]
    struct ScriptedClock {
        readings: Arc<Mutex<std::collections::VecDeque<i64>>>,
        last: Arc<Mutex<i64>>,
    }

    impl ScriptedClock {
        fn new(readings: &[i64]) -> Self {
            Self {
                readings: Arc::new(Mutex::new(readings.iter().copied().collect())),
                last: Arc::new(Mutex::new(readings.last().copied().unwrap_or_default())),
            }
        }

        fn set(&self, now_ms: i64) {
            self.readings.lock().unwrap().clear();
            *self.last.lock().unwrap() = now_ms;
        }
    }

    impl Clock for ScriptedClock {
        fn now_ms(&self) -> i64 {
            let mut last = self.last.lock().unwrap();
            if let Some(next) = self.readings.lock().unwrap().pop_front() {
                *last = next;
            }
            *last
        }
    }

    #[derive(Clone)]
    struct DeferQueue {
        interval: Duration,
        renewals: Arc<std::sync::atomic::AtomicUsize>,
        fail_renewal: Arc<std::sync::atomic::AtomicBool>,
        acks: Arc<std::sync::atomic::AtomicUsize>,
        nacks: Arc<Mutex<Vec<Duration>>>,
    }

    impl DeferQueue {
        fn new(interval: Duration) -> Self {
            Self {
                interval,
                renewals: Arc::default(),
                fail_renewal: Arc::default(),
                acks: Arc::default(),
                nacks: Arc::default(),
            }
        }

        fn renewals(&self) -> usize {
            self.renewals.load(std::sync::atomic::Ordering::SeqCst)
        }

        fn acks(&self) -> usize {
            self.acks.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    impl EmailQueue for DeferQueue {
        async fn enqueue(
            &self,
            _: EmailId,
            _: &Envelope,
        ) -> Result<EnqueueOutcome, EmailQueueError> {
            unimplemented!()
        }

        async fn dequeue(&self) -> Result<DequeuedEmail, EmailQueueError> {
            std::future::pending().await
        }

        async fn ack(&self, _: AckToken) -> Result<(), EmailQueueError> {
            self.acks.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }

        async fn nack(&self, _: AckToken, delay: Duration) -> Result<(), EmailQueueError> {
            self.nacks.lock().unwrap().push(delay);
            Ok(())
        }

        async fn in_progress(&self, _: &AckToken) -> Result<(), EmailQueueError> {
            if self.fail_renewal.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(EmailQueueError::Storage {
                    source: anyhow::anyhow!("lease lost"),
                });
            }
            self.renewals
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }

        fn renew_interval(&self) -> Duration {
            self.interval
        }
    }

    #[derive(Clone, Copy)]
    enum Fence {
        Proceed,
        Cancelled,
        Failing,
    }

    #[derive(Clone)]
    struct FenceRepository {
        fence: Fence,
        starts: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl EmailRepository for FenceRepository {
        async fn save(&self, _: EmailId, _: &Envelope) -> Result<SaveResult, EmailRepositoryError> {
            unimplemented!()
        }

        async fn list_all_attachment_blobs(&self) -> Result<Vec<BlobRef>, EmailRepositoryError> {
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

        async fn set_enqueue_state(
            &self,
            _: EmailId,
            _: &str,
            _: EnqueueState,
        ) -> Result<bool, EmailRepositoryError> {
            Ok(true)
        }

        async fn delete(&self, _: EmailId) -> Result<(), EmailRepositoryError> {
            unimplemented!()
        }
    }

    impl EmailCancellation for FenceRepository {
        async fn cancel(
            &self,
            _: EmailId,
            _: Option<&str>,
        ) -> Result<CancelOutcome, EmailRepositoryError> {
            unimplemented!()
        }

        async fn start_delivery(&self, _: EmailId) -> Result<DeliveryStart, EmailRepositoryError> {
            self.starts
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            match self.fence {
                Fence::Proceed => Ok(DeliveryStart::Proceed),
                Fence::Cancelled => Ok(DeliveryStart::Cancelled),
                Fence::Failing => Err(EmailRepositoryError::Storage {
                    source: anyhow::anyhow!("db down"),
                }),
            }
        }
    }

    #[derive(Clone)]
    struct CountingProcessor {
        fail: bool,
        executed: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl ProcessQueuedEmailUseCase for CountingProcessor {
        async fn execute(
            &self,
            _: EmailId,
            _: Envelope,
        ) -> Result<ProcessedEmail, ProcessQueuedEmailError> {
            self.executed
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if self.fail {
                return Err(ProcessQueuedEmailError::Send(
                    catapulte_domain::port::email_sender::SendError::Send {
                        sender_name: SenderName::new("primary"),
                        permanent: false,
                        source: anyhow::anyhow!("temporary failure"),
                    },
                ));
            }
            Ok(processed("sender"))
        }
    }

    #[derive(Clone)]
    struct SchedState {
        queue: DeferQueue,
        publisher: CapturingEventPublisher,
        repository: FenceRepository,
        processor: CountingProcessor,
    }

    impl SchedState {
        fn new(interval: Duration, fence: Fence, fail: bool) -> Self {
            Self {
                queue: DeferQueue::new(interval),
                publisher: CapturingEventPublisher::default(),
                repository: FenceRepository {
                    fence,
                    starts: Arc::default(),
                },
                processor: CountingProcessor {
                    fail,
                    executed: Arc::default(),
                },
            }
        }

        fn executed(&self) -> usize {
            self.processor
                .executed
                .load(std::sync::atomic::Ordering::SeqCst)
        }

        fn starts(&self) -> usize {
            self.repository
                .starts
                .load(std::sync::atomic::Ordering::SeqCst)
        }

        fn events(&self) -> Vec<LifecycleEvent> {
            self.publisher.events.lock().unwrap().clone()
        }

        fn nacks(&self) -> Vec<Duration> {
            self.queue.nacks.lock().unwrap().clone()
        }

        fn spawn_process(
            &self,
            clock: impl Clock + Clone,
            send_at_ms: Option<i64>,
        ) -> tokio::task::JoinHandle<()> {
            self.spawn_process_until(
                clock,
                send_at_ms,
                tokio_util::sync::CancellationToken::new(),
            )
        }

        fn spawn_process_until(
            &self,
            clock: impl Clock + Clone,
            send_at_ms: Option<i64>,
            cancel: tokio_util::sync::CancellationToken,
        ) -> tokio::task::JoinHandle<()> {
            let state = self.clone();
            tokio::spawn(async move {
                let mut envelope = sample_envelope();
                envelope.send_at_ms = send_at_ms;
                process_one(
                    &state,
                    &clock,
                    &cancel,
                    EmailId::default(),
                    envelope,
                    1,
                    AckToken::new(vec![0u8; 8]),
                    TraceCarrier::default(),
                )
                .await;
            })
        }
    }

    impl WorkerState for SchedState {
        fn process_queued_email(&self) -> &impl ProcessQueuedEmailUseCase {
            &self.processor
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

        fn email_repository(&self) -> &(impl EmailRepository + EmailCancellation) {
            &self.repository
        }
    }

    fn sending_attempts(events: &[LifecycleEvent]) -> Vec<u32> {
        events
            .iter()
            .filter_map(|e| match e {
                LifecycleEvent::Sending { attempt, .. } => Some(*attempt),
                _ => None,
            })
            .collect()
    }

    async fn settle() {
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test(start_paused = true)]
    async fn an_early_email_waits_and_renews_without_a_nack() {
        let state = SchedState::new(Duration::from_secs(3), Fence::Proceed, false);
        let clock = TimeClock::new();

        state
            .spawn_process(clock, Some(BASE_MS + 10_000))
            .await
            .unwrap();

        assert!(state.queue.renewals() >= 4, "{}", state.queue.renewals());
        assert!(state.nacks().is_empty());
        assert_eq!(state.executed(), 1);
        assert_eq!(sending_attempts(&state.events()), vec![1]);
        assert_eq!(state.starts(), 1);
        assert_eq!(state.queue.acks(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_backward_clock_jump_only_lengthens_the_wait() {
        let state = SchedState::new(Duration::from_secs(1), Fence::Proceed, false);
        let send_at = BASE_MS + 100_000;
        let clock = ScriptedClock::new(&[send_at - 3_000, send_at - 500, send_at - 2_000, send_at]);

        state.spawn_process(clock, Some(send_at)).await.unwrap();

        assert_eq!(state.queue.renewals(), 3, "one renewal per early reading");
        assert!(state.nacks().is_empty());
        assert_eq!(state.executed(), 1);
        assert_eq!(sending_attempts(&state.events()), vec![1]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_transient_failure_after_a_wait_is_nacked_with_the_first_backoff() {
        let state = SchedState::new(Duration::from_secs(3), Fence::Proceed, true);

        state
            .spawn_process(TimeClock::new(), Some(BASE_MS + 10_000))
            .await
            .unwrap();

        assert_eq!(state.nacks(), vec![super::backoff(1)]);
        let events = state.events();
        assert_eq!(sending_attempts(&events), vec![1]);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, LifecycleEvent::Retrying { attempt: 1, .. }))
        );
    }

    #[tokio::test(start_paused = true)]
    async fn an_email_scheduled_an_hour_ahead_is_never_nacked_and_runs_once_due() {
        let state = SchedState::new(Duration::from_secs(3), Fence::Proceed, false);
        let send_at = BASE_MS + 3_600_000;
        let clock = ScriptedClock::new(&[BASE_MS]);
        let handle = state.spawn_process(clock.clone(), Some(send_at));

        for _ in 0..40 {
            tokio::time::advance(Duration::from_secs(3)).await;
            settle().await;
        }

        assert!(!handle.is_finished());
        assert_eq!(state.executed(), 0);
        assert!(state.nacks().is_empty());
        assert!(state.events().is_empty());
        assert!(state.queue.renewals() >= 40);
        clock.set(send_at);
        tokio::time::advance(Duration::from_secs(3)).await;
        handle.await.unwrap();
        assert_eq!(state.executed(), 1);
        assert!(state.nacks().is_empty());
        assert_eq!(sending_attempts(&state.events()), vec![1]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_renewal_abandons_the_message_without_side_effects() {
        let state = SchedState::new(Duration::from_secs(3), Fence::Proceed, false);
        state
            .queue
            .fail_renewal
            .store(true, std::sync::atomic::Ordering::SeqCst);

        state
            .spawn_process(TimeClock::new(), Some(BASE_MS + 10_000))
            .await
            .unwrap();

        assert_eq!(state.executed(), 0);
        assert!(state.events().is_empty());
        assert_eq!(state.queue.acks(), 0);
        assert!(state.nacks().is_empty());
        assert_eq!(state.starts(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn a_cancelled_email_is_acknowledged_and_dropped() {
        let state = SchedState::new(Duration::from_secs(3), Fence::Cancelled, false);

        state
            .spawn_process(TimeClock::new(), Some(BASE_MS - 1_000))
            .await
            .unwrap();

        assert_eq!(state.queue.acks(), 1);
        assert_eq!(state.executed(), 0);
        assert!(state.events().is_empty());
        assert!(state.nacks().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn a_fence_error_nacks_without_events() {
        let state = SchedState::new(Duration::from_secs(3), Fence::Failing, false);

        state
            .spawn_process(TimeClock::new(), Some(BASE_MS - 1_000))
            .await
            .unwrap();

        assert_eq!(state.nacks(), vec![super::backoff(1)]);
        assert_eq!(state.queue.acks(), 0);
        assert_eq!(state.executed(), 0);
        assert!(state.events().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn an_email_without_a_send_time_skips_the_guard_and_the_fence() {
        let state = SchedState::new(Duration::from_secs(3), Fence::Cancelled, false);

        state.spawn_process(TimeClock::new(), None).await.unwrap();

        assert_eq!(state.starts(), 0);
        assert_eq!(state.queue.renewals(), 0);
        assert_eq!(state.executed(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_due_email_takes_the_fence_without_renewing() {
        let state = SchedState::new(Duration::from_secs(3), Fence::Proceed, false);

        state
            .spawn_process(TimeClock::new(), Some(BASE_MS - 1_000))
            .await
            .unwrap();

        assert_eq!(state.starts(), 1);
        assert_eq!(state.queue.renewals(), 0);
        assert_eq!(state.executed(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_during_a_wait_returns_without_side_effects() {
        let state = SchedState::new(Duration::from_secs(3), Fence::Proceed, false);
        let cancel = tokio_util::sync::CancellationToken::new();
        let handle = state.spawn_process_until(
            ScriptedClock::new(&[BASE_MS]),
            Some(BASE_MS + 3_600_000),
            cancel.clone(),
        );
        for _ in 0..3 {
            tokio::time::advance(Duration::from_secs(3)).await;
            settle().await;
        }
        assert!(!handle.is_finished());

        cancel.cancel();
        tokio::time::timeout(Duration::from_secs(1), handle)
            .await
            .expect("the wait must end on shutdown")
            .unwrap();

        assert_eq!(state.executed(), 0);
        assert!(state.events().is_empty());
        assert_eq!(state.queue.acks(), 0);
        assert!(state.nacks().is_empty());
        assert_eq!(state.starts(), 0);
    }
}
