use thiserror::Error;

use crate::entity::attachment::AttachmentRef;
use crate::entity::email::EmailId;
use crate::entity::envelope::Envelope;
use crate::entity::lifecycle_event::LifecycleEvent;
use crate::port::attachment_fetcher::{AttachmentFetchError, AttachmentFetcher};
use crate::port::attachment_store::{AttachmentReader, AttachmentStore};
use crate::port::email_queue::{EmailQueue, EmailQueueError};
use crate::port::email_repository::{EmailRepository, EmailRepositoryError, SaveResult};
use crate::port::event_publisher::EventPublisher;

pub enum AttachmentInput {
    Inline {
        filename: String,
        content_type: String,
        bytes: bytes::Bytes,
    },
    Remote {
        filename: String,
        content_type: String,
        url: url::Url,
    },
}

pub struct SubmitEmailInput {
    pub idempotency_key: Option<String>,
    pub subject: Option<String>,
    pub sender: String,
    pub recipients: Vec<(crate::entity::email::RecipientKind, String)>,
    pub body: crate::entity::body::BodySource,
    pub variables: serde_json::Map<String, serde_json::Value>,
    pub attachments: Vec<AttachmentInput>,
}

#[derive(Debug, Error)]
pub enum SubmitEmailError {
    #[error(transparent)]
    Persist(#[from] EmailRepositoryError),
    #[error(transparent)]
    Enqueue(#[from] EmailQueueError),
    #[error("attachment store failed")]
    AttachmentStore {
        #[source]
        source: anyhow::Error,
    },
    #[error("attachment fetch failed")]
    AttachmentFetch {
        #[source]
        source: anyhow::Error,
    },
}

pub trait SubmitEmailUseCase: Send + Sync + 'static {
    /// # Errors
    ///
    /// Returns `SubmitEmailError::Persist` when saving the envelope fails.
    /// Returns `SubmitEmailError::Enqueue` when enqueuing fails.
    /// Returns `SubmitEmailError::AttachmentStore` when blob upload fails.
    fn execute(
        &self,
        input: SubmitEmailInput,
    ) -> impl std::future::Future<Output = Result<EmailId, SubmitEmailError>> + Send;
}

pub struct SubmitEmailService<R, Q, P, A, F> {
    repository: R,
    queue: Q,
    event_publisher: P,
    attachment_store: A,
    attachment_fetcher: F,
}

impl<R, Q, P, A, F> SubmitEmailService<R, Q, P, A, F>
where
    R: EmailRepository,
    Q: EmailQueue,
    P: EventPublisher,
    A: AttachmentStore,
    F: AttachmentFetcher,
{
    #[must_use]
    pub fn new(
        repository: R,
        queue: Q,
        event_publisher: P,
        attachment_store: A,
        attachment_fetcher: F,
    ) -> Self {
        Self {
            repository,
            queue,
            event_publisher,
            attachment_store,
            attachment_fetcher,
        }
    }

    /// # Errors
    ///
    /// Returns `SubmitEmailError::Persist` when saving the envelope fails.
    /// Returns `SubmitEmailError::Enqueue` when enqueuing fails.
    /// Returns `SubmitEmailError::AttachmentStore` when blob upload fails.
    pub async fn execute(&self, input: SubmitEmailInput) -> Result<EmailId, SubmitEmailError> {
        let id = EmailId::default();

        // Reserve the row with an empty attachment list first; the final list is
        // patched in after blobs are written so the worker never sees stale refs.
        let envelope_for_reservation = Envelope {
            idempotency_key: input.idempotency_key.clone(),
            subject: input.subject.clone(),
            sender: input.sender.clone(),
            recipients: input.recipients.clone(),
            body: input.body.clone(),
            variables: input.variables.clone(),
            attachments: vec![],
        };

        let result = self.repository.save(id, &envelope_for_reservation).await?;
        match result {
            SaveResult::Duplicate(existing_id) => return Ok(existing_id),
            SaveResult::Created(_) => {}
        }

        let mut written_refs: Vec<AttachmentRef> = Vec::with_capacity(input.attachments.len());
        for att in &input.attachments {
            let (filename, content_type, reader) = match att {
                AttachmentInput::Inline {
                    filename,
                    content_type,
                    bytes,
                } => (
                    filename.clone(),
                    content_type.clone(),
                    Box::pin(std::io::Cursor::new(bytes.to_vec())) as AttachmentReader,
                ),
                AttachmentInput::Remote {
                    filename,
                    content_type,
                    url,
                } => match self.attachment_fetcher.fetch(url).await {
                    Ok(r) => (filename.clone(), content_type.clone(), r),
                    Err(fetch_err) => {
                        for r in &written_refs {
                            let _ = self.attachment_store.delete(&r.blob).await;
                        }
                        let _ = self.repository.delete(id).await;
                        return Err(SubmitEmailError::AttachmentFetch {
                            source: anyhow::Error::new(fetch_err),
                        });
                    }
                },
            };
            match self.attachment_store.put(reader).await {
                Ok(put_result) => {
                    written_refs.push(AttachmentRef {
                        filename,
                        content_type,
                        size_bytes: put_result.size_bytes,
                        blob: put_result.blob,
                    });
                }
                Err(store_err) => {
                    for r in &written_refs {
                        let _ = self.attachment_store.delete(&r.blob).await;
                    }
                    let _ = self.repository.delete(id).await;
                    return Err(SubmitEmailError::AttachmentStore {
                        source: anyhow::Error::new(store_err),
                    });
                }
            }
        }

        if !written_refs.is_empty() {
            if let Err(e) = self.repository.set_attachments(id, &written_refs).await {
                for r in &written_refs {
                    let _ = self.attachment_store.delete(&r.blob).await;
                }
                let _ = self.repository.delete(id).await;
                return Err(SubmitEmailError::Persist(e));
            }
        }

        let envelope = Envelope {
            idempotency_key: input.idempotency_key,
            subject: input.subject,
            sender: input.sender,
            recipients: input.recipients,
            body: input.body,
            variables: input.variables,
            attachments: written_refs,
        };

        self.queue.enqueue(id, &envelope).await?;
        if let Err(e) = self
            .event_publisher
            .publish(&LifecycleEvent::Queued { id })
            .await
        {
            tracing::warn!(error = %e, email_id = %id.as_uuid(), "failed to publish queued event");
        }
        Ok(id)
    }
}

impl<R, Q, P, A, F> SubmitEmailUseCase for SubmitEmailService<R, Q, P, A, F>
where
    R: EmailRepository + Send + Sync + 'static,
    Q: EmailQueue + Send + Sync + 'static,
    P: EventPublisher + Send + Sync + 'static,
    A: AttachmentStore + Send + Sync + 'static,
    F: AttachmentFetcher + Send + Sync + 'static,
{
    fn execute(
        &self,
        input: SubmitEmailInput,
    ) -> impl std::future::Future<Output = Result<EmailId, SubmitEmailError>> + Send {
        Self::execute(self, input)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use crate::entity::attachment::{AttachmentRef, BlobRef};
    use crate::entity::body::Plain;
    use crate::entity::email::{EmailId, RecipientKind};
    use crate::entity::envelope::Envelope;
    use crate::entity::lifecycle_event::LifecycleEvent;
    use crate::port::attachment_fetcher::{AttachmentFetchError, AttachmentFetcher};
    use crate::port::attachment_store::{
        AttachmentReader, AttachmentStore, AttachmentStoreError, PutResult,
    };
    use crate::port::email_queue::{EmailQueue, EmailQueueError};
    use crate::port::email_repository::{EmailRepository, EmailRepositoryError, SaveResult};
    use crate::port::event_publisher::{EventPublisher, EventPublisherError};

    use super::{AttachmentInput, SubmitEmailError, SubmitEmailInput, SubmitEmailService};

    fn make_input(sender: &str) -> SubmitEmailInput {
        SubmitEmailInput {
            idempotency_key: None,
            subject: None,
            sender: sender.into(),
            recipients: vec![(RecipientKind::To, "to@example.com".into())],
            body: crate::entity::body::BodySource::Plain(
                Plain::try_new(Some("hello".into()), None).unwrap(),
            ),
            variables: serde_json::Map::new(),
            attachments: vec![],
        }
    }

    #[derive(Clone)]
    struct FakeRepository {
        saved: Arc<Mutex<Vec<(EmailId, String)>>>,
    }

    impl FakeRepository {
        fn new() -> Self {
            Self {
                saved: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    #[allow(async_fn_in_trait)]
    impl EmailRepository for FakeRepository {
        async fn save(
            &self,
            id: EmailId,
            envelope: &Envelope,
        ) -> Result<SaveResult, EmailRepositoryError> {
            self.saved
                .lock()
                .unwrap()
                .push((id, envelope.sender.clone()));
            Ok(SaveResult::Created(id))
        }

        async fn list_emails(
            &self,
            _params: crate::port::email_repository::ListEmailsParams,
        ) -> Result<Vec<crate::port::email_repository::EmailRecord>, EmailRepositoryError> {
            Ok(vec![])
        }

        async fn set_attachments(
            &self,
            _id: EmailId,
            _attachments: &[AttachmentRef],
        ) -> Result<(), EmailRepositoryError> {
            Ok(())
        }

        async fn delete(&self, _id: EmailId) -> Result<(), EmailRepositoryError> {
            Ok(())
        }
    }

    struct FailingRepository;

    #[allow(async_fn_in_trait)]
    impl EmailRepository for FailingRepository {
        async fn save(
            &self,
            _id: EmailId,
            _envelope: &Envelope,
        ) -> Result<SaveResult, EmailRepositoryError> {
            Err(EmailRepositoryError::Storage {
                source: anyhow::anyhow!("storage down"),
            })
        }

        async fn list_emails(
            &self,
            _params: crate::port::email_repository::ListEmailsParams,
        ) -> Result<Vec<crate::port::email_repository::EmailRecord>, EmailRepositoryError> {
            Ok(vec![])
        }

        async fn set_attachments(
            &self,
            _id: EmailId,
            _attachments: &[AttachmentRef],
        ) -> Result<(), EmailRepositoryError> {
            Err(EmailRepositoryError::Storage {
                source: anyhow::anyhow!("storage down"),
            })
        }

        async fn delete(&self, _id: EmailId) -> Result<(), EmailRepositoryError> {
            Err(EmailRepositoryError::Storage {
                source: anyhow::anyhow!("storage down"),
            })
        }
    }

    struct DuplicatingRepository {
        existing_id: EmailId,
    }

    #[allow(async_fn_in_trait)]
    impl EmailRepository for DuplicatingRepository {
        async fn save(
            &self,
            _id: EmailId,
            _envelope: &Envelope,
        ) -> Result<SaveResult, EmailRepositoryError> {
            Ok(SaveResult::Duplicate(self.existing_id))
        }

        async fn list_emails(
            &self,
            _params: crate::port::email_repository::ListEmailsParams,
        ) -> Result<Vec<crate::port::email_repository::EmailRecord>, EmailRepositoryError> {
            Ok(vec![])
        }

        async fn set_attachments(
            &self,
            _id: EmailId,
            _attachments: &[AttachmentRef],
        ) -> Result<(), EmailRepositoryError> {
            Ok(())
        }

        async fn delete(&self, _id: EmailId) -> Result<(), EmailRepositoryError> {
            Ok(())
        }
    }

    #[derive(Clone)]
    struct FakeQueue {
        enqueued: Arc<Mutex<Vec<EmailId>>>,
    }

    impl FakeQueue {
        fn new() -> Self {
            Self {
                enqueued: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    #[allow(async_fn_in_trait)]
    impl EmailQueue for FakeQueue {
        async fn enqueue(&self, id: EmailId, _envelope: &Envelope) -> Result<(), EmailQueueError> {
            self.enqueued.lock().unwrap().push(id);
            Ok(())
        }

        async fn dequeue(
            &self,
        ) -> Result<(EmailId, Envelope, u32, crate::port::email_queue::AckToken), EmailQueueError>
        {
            std::future::pending().await
        }

        async fn ack(
            &self,
            _token: crate::port::email_queue::AckToken,
        ) -> Result<(), EmailQueueError> {
            Ok(())
        }

        async fn nack(
            &self,
            _token: crate::port::email_queue::AckToken,
            _delay: std::time::Duration,
        ) -> Result<(), EmailQueueError> {
            Ok(())
        }
    }

    struct FailingQueue;

    #[allow(async_fn_in_trait)]
    impl EmailQueue for FailingQueue {
        async fn enqueue(&self, _id: EmailId, _envelope: &Envelope) -> Result<(), EmailQueueError> {
            Err(EmailQueueError::Storage {
                source: anyhow::anyhow!("queue down"),
            })
        }

        async fn dequeue(
            &self,
        ) -> Result<(EmailId, Envelope, u32, crate::port::email_queue::AckToken), EmailQueueError>
        {
            std::future::pending().await
        }

        async fn ack(
            &self,
            _token: crate::port::email_queue::AckToken,
        ) -> Result<(), EmailQueueError> {
            Ok(())
        }

        async fn nack(
            &self,
            _token: crate::port::email_queue::AckToken,
            _delay: std::time::Duration,
        ) -> Result<(), EmailQueueError> {
            Ok(())
        }
    }

    #[derive(Clone)]
    struct FakeEventPublisher {
        published: Arc<Mutex<Vec<LifecycleEvent>>>,
    }

    impl FakeEventPublisher {
        fn new() -> Self {
            Self {
                published: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    #[allow(async_fn_in_trait)]
    impl EventPublisher for FakeEventPublisher {
        async fn publish(&self, event: &LifecycleEvent) -> Result<(), EventPublisherError> {
            self.published.lock().unwrap().push(event.clone());
            Ok(())
        }
    }

    struct FailingEventPublisher;

    #[allow(async_fn_in_trait)]
    impl EventPublisher for FailingEventPublisher {
        async fn publish(&self, _event: &LifecycleEvent) -> Result<(), EventPublisherError> {
            Err(EventPublisherError::Publish {
                source: anyhow::anyhow!("publish failed"),
            })
        }
    }

    #[derive(Clone)]
    struct FakeAttachmentStore {
        put_count: Arc<Mutex<usize>>,
    }

    impl FakeAttachmentStore {
        fn new() -> Self {
            Self {
                put_count: Arc::new(Mutex::new(0)),
            }
        }
    }

    #[allow(async_fn_in_trait)]
    impl AttachmentStore for FakeAttachmentStore {
        async fn put(
            &self,
            mut reader: AttachmentReader,
        ) -> Result<PutResult, AttachmentStoreError> {
            use tokio::io::AsyncReadExt;
            let mut buf = Vec::new();
            reader
                .read_to_end(&mut buf)
                .await
                .map_err(|e| AttachmentStoreError::Io {
                    source: anyhow::Error::new(e),
                })?;
            let size_bytes = buf.len() as u64;
            *self.put_count.lock().unwrap() += 1;
            Ok(PutResult {
                blob: BlobRef {
                    backend: "fake".into(),
                    key: format!("fake-key-{}", uuid::Uuid::now_v7().simple()),
                },
                size_bytes,
            })
        }

        async fn get(
            &self,
            _blob: &crate::entity::attachment::BlobRef,
        ) -> Result<AttachmentReader, AttachmentStoreError> {
            Ok(Box::pin(std::io::Cursor::new(b"fake content".to_vec())))
        }

        async fn delete(
            &self,
            _blob: &crate::entity::attachment::BlobRef,
        ) -> Result<(), AttachmentStoreError> {
            Ok(())
        }
    }

    struct FailingAttachmentStore;

    #[allow(async_fn_in_trait)]
    impl AttachmentStore for FailingAttachmentStore {
        async fn put(&self, _reader: AttachmentReader) -> Result<PutResult, AttachmentStoreError> {
            Err(AttachmentStoreError::Io {
                source: anyhow::anyhow!("store down"),
            })
        }

        async fn get(
            &self,
            _blob: &crate::entity::attachment::BlobRef,
        ) -> Result<AttachmentReader, AttachmentStoreError> {
            Err(AttachmentStoreError::NotFound)
        }

        async fn delete(
            &self,
            _blob: &crate::entity::attachment::BlobRef,
        ) -> Result<(), AttachmentStoreError> {
            Ok(())
        }
    }

    struct FakeFetcher;

    #[allow(async_fn_in_trait)]
    impl AttachmentFetcher for FakeFetcher {
        async fn fetch(&self, _url: &url::Url) -> Result<AttachmentReader, AttachmentFetchError> {
            Ok(Box::pin(std::io::Cursor::new(
                b"fake remote bytes".to_vec(),
            )))
        }
    }

    struct FailingFetcher;

    #[allow(async_fn_in_trait)]
    impl AttachmentFetcher for FailingFetcher {
        async fn fetch(&self, _url: &url::Url) -> Result<AttachmentReader, AttachmentFetchError> {
            Err(AttachmentFetchError::Fetch {
                source: anyhow::anyhow!("fetch failed"),
            })
        }
    }

    #[tokio::test]
    async fn execute_persists_the_envelope() {
        let repo = FakeRepository::new();
        let queue = FakeQueue::new();
        let service = SubmitEmailService::new(
            repo.clone(),
            queue.clone(),
            FakeEventPublisher::new(),
            FakeAttachmentStore::new(),
            FakeFetcher,
        );
        let id = service
            .execute(make_input("sender@example.com"))
            .await
            .unwrap();
        let saved = repo.saved.lock().unwrap();
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].0, id);
        assert_eq!(saved[0].1, "sender@example.com");
    }

    #[tokio::test]
    async fn execute_enqueues_the_email() {
        let repo = FakeRepository::new();
        let queue = FakeQueue::new();
        let service = SubmitEmailService::new(
            repo.clone(),
            queue.clone(),
            FakeEventPublisher::new(),
            FakeAttachmentStore::new(),
            FakeFetcher,
        );
        let id = service
            .execute(make_input("sender@example.com"))
            .await
            .unwrap();
        let enqueued = queue.enqueued.lock().unwrap();
        assert_eq!(enqueued.len(), 1);
        assert_eq!(enqueued[0], id);
    }

    #[tokio::test]
    async fn persistence_failure_propagates_as_submit_email_error() {
        let queue = FakeQueue::new();
        let service = SubmitEmailService::new(
            FailingRepository,
            queue.clone(),
            FakeEventPublisher::new(),
            FakeAttachmentStore::new(),
            FakeFetcher,
        );
        let err = service
            .execute(make_input("sender@example.com"))
            .await
            .unwrap_err();
        assert!(matches!(err, SubmitEmailError::Persist(_)));
        assert!(queue.enqueued.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn enqueue_failure_propagates_as_submit_email_error() {
        let repo = FakeRepository::new();
        let service = SubmitEmailService::new(
            repo.clone(),
            FailingQueue,
            FakeEventPublisher::new(),
            FakeAttachmentStore::new(),
            FakeFetcher,
        );
        let err = service
            .execute(make_input("sender@example.com"))
            .await
            .unwrap_err();
        assert!(matches!(err, SubmitEmailError::Enqueue(_)));
        assert_eq!(repo.saved.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn execute_returns_a_fresh_id_each_call() {
        let repo = FakeRepository::new();
        let queue = FakeQueue::new();
        let service = SubmitEmailService::new(
            repo.clone(),
            queue.clone(),
            FakeEventPublisher::new(),
            FakeAttachmentStore::new(),
            FakeFetcher,
        );
        let id1 = service
            .execute(make_input("sender@example.com"))
            .await
            .unwrap();
        let id2 = service
            .execute(make_input("sender@example.com"))
            .await
            .unwrap();
        assert_ne!(id1, id2);
    }

    #[tokio::test]
    async fn duplicate_idempotency_key_returns_existing_id_without_enqueue() {
        let existing_id = EmailId::default();
        let queue = FakeQueue::new();
        let service = SubmitEmailService::new(
            DuplicatingRepository { existing_id },
            queue.clone(),
            FakeEventPublisher::new(),
            FakeAttachmentStore::new(),
            FakeFetcher,
        );
        let returned_id = service
            .execute(make_input("sender@example.com"))
            .await
            .unwrap();
        assert_eq!(returned_id, existing_id);
        assert!(queue.enqueued.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn execute_emits_queued_event_after_enqueue() {
        let repo = FakeRepository::new();
        let queue = FakeQueue::new();
        let spy = FakeEventPublisher::new();
        let service = SubmitEmailService::new(
            repo.clone(),
            queue.clone(),
            spy.clone(),
            FakeAttachmentStore::new(),
            FakeFetcher,
        );
        let id = service
            .execute(make_input("sender@example.com"))
            .await
            .unwrap();
        let events = spy.published.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0], LifecycleEvent::Queued { id });
    }

    #[tokio::test]
    async fn duplicate_idempotency_key_does_not_emit_queued_event() {
        let existing_id = EmailId::default();
        let queue = FakeQueue::new();
        let spy = FakeEventPublisher::new();
        let service = SubmitEmailService::new(
            DuplicatingRepository { existing_id },
            queue.clone(),
            spy.clone(),
            FakeAttachmentStore::new(),
            FakeFetcher,
        );
        service
            .execute(make_input("sender@example.com"))
            .await
            .unwrap();
        assert!(spy.published.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn publish_failure_does_not_fail_submit() {
        let repo = FakeRepository::new();
        let queue = FakeQueue::new();
        let service = SubmitEmailService::new(
            repo.clone(),
            queue.clone(),
            FailingEventPublisher,
            FakeAttachmentStore::new(),
            FakeFetcher,
        );
        let result = service.execute(make_input("sender@example.com")).await;
        assert!(
            result.is_ok(),
            "publish failure must not propagate as error"
        );
        assert_eq!(
            queue.enqueued.lock().unwrap().len(),
            1,
            "email must still be enqueued"
        );
    }

    #[tokio::test]
    async fn attachment_store_failure_propagates_as_error_and_cleans_up() {
        let repo = FakeRepository::new();
        let queue = FakeQueue::new();
        let mut input = make_input("sender@example.com");
        input.attachments.push(AttachmentInput::Inline {
            filename: "file.txt".into(),
            content_type: "text/plain".into(),
            bytes: bytes::Bytes::from_static(b"hello"),
        });
        let service = SubmitEmailService::new(
            repo.clone(),
            queue.clone(),
            FakeEventPublisher::new(),
            FailingAttachmentStore,
            FakeFetcher,
        );
        let err = service.execute(input).await.unwrap_err();
        assert!(matches!(err, SubmitEmailError::AttachmentStore { .. }));
        assert!(queue.enqueued.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn attachments_are_uploaded_before_enqueue() {
        let repo = FakeRepository::new();
        let queue = FakeQueue::new();
        let store = FakeAttachmentStore::new();
        let put_count = store.put_count.clone();
        let mut input = make_input("sender@example.com");
        input.attachments.push(AttachmentInput::Inline {
            filename: "test.txt".into(),
            content_type: "text/plain".into(),
            bytes: bytes::Bytes::from_static(b"attachment content"),
        });
        let service = SubmitEmailService::new(
            repo.clone(),
            queue.clone(),
            FakeEventPublisher::new(),
            store,
            FakeFetcher,
        );
        service.execute(input).await.unwrap();
        assert_eq!(*put_count.lock().unwrap(), 1);
        assert_eq!(queue.enqueued.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn execute_writes_remote_attachment_via_fetcher() {
        let repo = FakeRepository::new();
        let queue = FakeQueue::new();
        let store = FakeAttachmentStore::new();
        let put_count = store.put_count.clone();
        let mut input = make_input("sender@example.com");
        input.attachments.push(AttachmentInput::Remote {
            filename: "remote.txt".into(),
            content_type: "text/plain".into(),
            url: url::Url::parse("https://example.com/file.txt").unwrap(),
        });
        let service = SubmitEmailService::new(
            repo.clone(),
            queue.clone(),
            FakeEventPublisher::new(),
            store,
            FakeFetcher,
        );
        service.execute(input).await.unwrap();
        assert_eq!(*put_count.lock().unwrap(), 1);
        assert_eq!(queue.enqueued.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn fetcher_failure_compensates_and_errors() {
        let repo = FakeRepository::new();
        let queue = FakeQueue::new();
        let mut input = make_input("sender@example.com");
        input.attachments.push(AttachmentInput::Remote {
            filename: "remote.txt".into(),
            content_type: "text/plain".into(),
            url: url::Url::parse("https://example.com/file.txt").unwrap(),
        });
        let service = SubmitEmailService::new(
            repo.clone(),
            queue.clone(),
            FakeEventPublisher::new(),
            FakeAttachmentStore::new(),
            FailingFetcher,
        );
        let err = service.execute(input).await.unwrap_err();
        assert!(matches!(err, SubmitEmailError::AttachmentFetch { .. }));
        assert!(queue.enqueued.lock().unwrap().is_empty());
    }
}
