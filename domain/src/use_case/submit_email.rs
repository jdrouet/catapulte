use thiserror::Error;

use crate::entity::attachment::AttachmentRef;
use crate::entity::email::{EmailId, bare_address};
use crate::entity::envelope::Envelope;
use crate::entity::lifecycle_event::LifecycleEvent;
use crate::entity::suppression::suppression_key;
use crate::port::attachment_fetcher::AttachmentFetcher;
use crate::port::attachment_store::{AttachmentReader, AttachmentStore};
use crate::port::email_queue::{EmailQueue, EmailQueueError, EnqueueOutcome};
use crate::port::email_repository::{
    DuplicateState, EmailRepository, EmailRepositoryError, EnqueueState, SaveResult,
};
use crate::port::event_publisher::EventPublisher;
use crate::port::suppression_repository::{
    NoopSuppressionList, SuppressionRepository, SuppressionRepositoryError,
};

pub enum AttachmentInput {
    Inline {
        filename: String,
        content_type: String,
        bytes: bytes::Bytes,
        content_id: Option<String>,
    },
    Stream {
        filename: String,
        content_type: String,
        reader: crate::port::attachment_store::AttachmentReader,
        content_id: Option<String>,
    },
    Remote {
        filename: String,
        content_type: String,
        url: url::Url,
        content_id: Option<String>,
    },
}

pub struct SubmitEmailInput {
    /// Set by the HTTP middleware (or `""` for NATS), never taken from the request body.
    pub owner: String,
    /// Sender domains this caller may use, intersected with the global allow-list. `None`
    /// adds no restriction; `Some` of an empty set denies every sender.
    pub sender_domains: Option<std::collections::HashSet<String>>,
    pub idempotency_key: Option<String>,
    pub correlation_id: Option<String>,
    pub subject: Option<String>,
    pub sender: String,
    pub recipients: Vec<(crate::entity::email::RecipientKind, String)>,
    pub body: crate::entity::body::BodySource,
    pub variables: serde_json::Map<String, serde_json::Value>,
    pub attachments: Vec<AttachmentInput>,
    pub reply_to: Vec<String>,
    pub headers: Vec<(String, String)>,
    pub unsubscribe: Option<crate::entity::envelope::Unsubscribe>,
    pub tags: Vec<String>,
    pub metadata: std::collections::BTreeMap<String, String>,
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
    #[error("estimated message size exceeds the configured maximum of {max_bytes} bytes")]
    MessageTooLarge { max_bytes: u64 },
    #[error("submission with this idempotency key is incomplete; retry later")]
    ReservationIncomplete { id: EmailId },
    #[error("sender domain {domain} is not allowed")]
    SenderDomainNotAllowed { domain: String },
    #[error("every recipient is on the suppression list")]
    AllRecipientsSuppressed,
    #[error(transparent)]
    Suppression(#[from] SuppressionRepositoryError),
}

impl SubmitEmailError {
    /// Returns true when the caller should retry later; false when the error
    /// is permanent for the given input and retrying won't help.
    #[must_use]
    pub fn is_transient(&self) -> bool {
        match self {
            // Storage / queue issues are infrastructure-level and likely transient.
            // Attachment store I/O could be either; treat as transient (disk full
            // recovers; remote storage hiccup recovers).
            Self::Persist(_)
            | Self::Enqueue(_)
            | Self::AttachmentStore { .. }
            | Self::Suppression(_) => true,
            // Remote URL fetch errors are almost always permanent for the given
            // URL (404, 410, blocked domain, oversize). Don't retry.
            Self::AttachmentFetch { .. }
            | Self::MessageTooLarge { .. }
            | Self::SenderDomainNotAllowed { .. }
            | Self::AllRecipientsSuppressed
            | Self::ReservationIncomplete { .. } => false,
        }
    }
}

pub trait SubmitEmailUseCase: Send + Sync + 'static {
    /// # Errors
    ///
    /// Returns `SubmitEmailError::Persist` when saving the envelope fails.
    /// Returns `SubmitEmailError::Enqueue` when enqueuing fails.
    /// Returns `SubmitEmailError::AttachmentStore` when blob upload fails.
    /// Returns `SubmitEmailError::MessageTooLarge` when the estimated size exceeds the limit.
    /// Returns `SubmitEmailError::SenderDomainNotAllowed` when the sender domain is not allowed.
    /// Returns `SubmitEmailError::ReservationIncomplete` when the idempotency key matches a
    /// submission that never completed.
    /// Returns `SubmitEmailError::AllRecipientsSuppressed` when no recipient is left after
    /// removing suppressed ones.
    fn execute(
        &self,
        input: SubmitEmailInput,
    ) -> impl std::future::Future<Output = Result<EmailId, SubmitEmailError>> + Send;
}

/// Limits applied at submit time.
#[derive(Clone, Debug)]
pub struct SubmitLimits {
    /// Normalized domains allowed as sender, matched exactly (no subdomains). `None` allows
    /// any domain; `Some` of an empty set denies every sender.
    pub allowed_sender_domains: Option<std::collections::HashSet<String>>,
    /// Early-rejection threshold for the estimated wire size; the SMTP adapter enforces the
    /// final size on the rendered message.
    pub max_message_bytes: u64,
}

impl Default for SubmitLimits {
    fn default() -> Self {
        Self {
            max_message_bytes: 25 * 1024 * 1024,
            allowed_sender_domains: None,
        }
    }
}

/// Lowercase punycode form of a domain without a trailing dot; `None` when it cannot be
/// converted or is not a valid hostname (at most one trailing dot is stripped; labels of 1-63
/// letters, digits and inner hyphens; at most 253 bytes).
#[must_use]
pub fn normalize_sender_domain(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let domain = idna::domain_to_ascii(trimmed.strip_suffix('.').unwrap_or(trimmed))
        .ok()?
        .to_ascii_lowercase();
    let valid = domain.len() <= 253
        && domain.split('.').all(|label| {
            (1..=63).contains(&label.len())
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        });
    valid.then_some(domain)
}

/// Normalizes configured sender domains into a set.
///
/// # Errors
///
/// Returns an error naming the first entry that cannot be converted.
pub fn normalize_sender_domains<S: AsRef<str>>(
    domains: impl IntoIterator<Item = S>,
) -> anyhow::Result<std::collections::HashSet<String>> {
    domains
        .into_iter()
        .map(|d| {
            normalize_sender_domain(d.as_ref())
                .ok_or_else(|| anyhow::anyhow!("invalid sender domain {:?}", d.as_ref()))
        })
        .collect()
}

/// Renders an error and its sources as `a: b: c`.
pub(crate) fn error_chain(e: &dyn std::error::Error) -> String {
    let mut out = e.to_string();
    let mut source = e.source();
    while let Some(s) = source {
        out.push_str(": ");
        out.push_str(&s.to_string());
        source = s.source();
    }
    out
}

/// Estimated base64 wire size of the attachments plus inline body. Excludes template and
/// variable growth, rendering, MIME headers, boundaries and line wrapping.
fn estimate_message_bytes(
    attachments: &[AttachmentRef],
    body: &crate::entity::body::BodySource,
) -> u64 {
    use crate::entity::body::{BodySource, MjmlSource};

    let body_len = match body {
        BodySource::Plain(p) => p.text().map_or(0, str::len) + p.html().map_or(0, str::len),
        BodySource::Mjml(MjmlSource::Inline(s)) => s.len(),
        BodySource::Mjml(_) => 0,
    };
    let total = attachments
        .iter()
        .fold(body_len as u128, |acc, a| acc + u128::from(a.size_bytes));
    u64::try_from(total * 4 / 3).unwrap_or(u64::MAX)
}

pub struct SubmitEmailService<R, Q, P, A, F, X = NoopSuppressionList> {
    repository: R,
    queue: Q,
    event_publisher: P,
    attachment_store: A,
    attachment_fetcher: F,
    suppressions: X,
    limits: SubmitLimits,
}

impl<R, Q, P, A, F> SubmitEmailService<R, Q, P, A, F, NoopSuppressionList> {
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
            suppressions: NoopSuppressionList,
            limits: SubmitLimits::default(),
        }
    }
}

impl<R, Q, P, A, F, X> SubmitEmailService<R, Q, P, A, F, X> {
    /// Drops suppressed recipients at submit time.
    #[must_use]
    pub fn with_suppression_list<Y>(self, suppressions: Y) -> SubmitEmailService<R, Q, P, A, F, Y> {
        SubmitEmailService {
            repository: self.repository,
            queue: self.queue,
            event_publisher: self.event_publisher,
            attachment_store: self.attachment_store,
            attachment_fetcher: self.attachment_fetcher,
            suppressions,
            limits: self.limits,
        }
    }

    #[must_use]
    pub fn with_limits(mut self, limits: SubmitLimits) -> Self {
        self.limits = limits;
        self
    }
}

impl<R, Q, P, A, F, X> SubmitEmailService<R, Q, P, A, F, X>
where
    R: EmailRepository,
    Q: EmailQueue,
    P: EventPublisher,
    A: AttachmentStore,
    F: AttachmentFetcher,
    X: SuppressionRepository,
{
    /// Removes the stored blobs and the reserved row, and returns the error to report.
    ///
    /// Blob failures are logged and ignored (GC sweeps orphans). If the row delete fails the
    /// reservation survives, so the transient error replaces `original`: no permanent rejection.
    async fn rollback(
        &self,
        id: EmailId,
        written: &[AttachmentRef],
        original: SubmitEmailError,
    ) -> SubmitEmailError {
        for r in written {
            if let Err(e) = self.attachment_store.delete(&r.blob).await {
                tracing::warn!(
                    error = %error_chain(&e),
                    blob_key = %r.blob.key,
                    "failed to delete attachment blob during rollback"
                );
            }
        }
        match self.repository.delete(id).await {
            Ok(()) => original,
            Err(e) => {
                tracing::error!(
                    error = %error_chain(&original),
                    email_id = %id.as_uuid(),
                    "rollback failed; original submit error"
                );
                SubmitEmailError::Persist(e)
            }
        }
    }

    /// The sender must pass the global allow-list and the caller's own list; each applies
    /// only when set.
    fn check_sender_allowed(
        &self,
        sender: &str,
        caller_domains: Option<&std::collections::HashSet<String>>,
    ) -> Result<(), SubmitEmailError> {
        let domain = bare_address(sender).rsplit_once('@').map_or("", |(_, d)| d);
        let normalized = normalize_sender_domain(domain);
        let allowed = [self.limits.allowed_sender_domains.as_ref(), caller_domains]
            .into_iter()
            .flatten()
            .all(|set| normalized.as_ref().is_some_and(|d| set.contains(d)));
        if allowed {
            Ok(())
        } else {
            Err(SubmitEmailError::SenderDomainNotAllowed {
                domain: domain.to_owned(),
            })
        }
    }

    /// Handles a submission whose idempotency key already exists. A recovery republish is
    /// checked against the stored sender; a plain duplicate only returns the existing id.
    async fn resolve_duplicate(
        &self,
        id: EmailId,
        state: DuplicateState,
        recovery: Option<Envelope>,
        caller_domains: Option<&std::collections::HashSet<String>>,
    ) -> Result<EmailId, SubmitEmailError> {
        if state == DuplicateState::Reserved {
            return Err(SubmitEmailError::ReservationIncomplete { id });
        }
        if let Some(envelope) = recovery {
            self.check_sender_allowed(&envelope.sender, caller_domains)?;
            if self.queue.enqueue(id, &envelope).await? == EnqueueOutcome::Inserted {
                self.mark_enqueued_and_announce(id, &envelope).await;
            }
        }
        Ok(id)
    }

    /// Records the confirmed publish and emits `queued`. A failed state write is only
    /// logged: the row stays `publishable` and a same-key retry republishes, deduplicated
    /// by the queue.
    async fn mark_enqueued_and_announce(&self, id: EmailId, envelope: &Envelope) {
        if let Err(e) = self
            .repository
            .set_enqueue_state(id, &envelope.owner, EnqueueState::Enqueued)
            .await
        {
            tracing::warn!(
                error = %error_chain(&e),
                email_id = %id.as_uuid(),
                "failed to record enqueued state"
            );
        }
        if let Err(e) = self
            .event_publisher
            .publish(&LifecycleEvent::Queued {
                id,
                context: envelope.event_context(),
            })
            .await
        {
            tracing::warn!(error = %e, email_id = %id.as_uuid(), "failed to publish queued event");
        }
    }

    async fn announce_suppressed(&self, id: EmailId, envelope: &Envelope, addresses: Vec<String>) {
        if addresses.is_empty() {
            return;
        }
        if let Err(e) = self
            .event_publisher
            .publish(&LifecycleEvent::Suppressed {
                id,
                addresses,
                context: envelope.event_context(),
            })
            .await
        {
            tracing::warn!(error = %e, email_id = %id.as_uuid(), "failed to publish suppressed event");
        }
    }

    /// # Errors
    ///
    /// Returns `SubmitEmailError::Persist` when saving the envelope fails.
    /// Returns `SubmitEmailError::Enqueue` when enqueuing fails.
    /// Returns `SubmitEmailError::AttachmentStore` when blob upload fails.
    /// Returns `SubmitEmailError::SenderDomainNotAllowed` when the sender domain is not allowed.
    /// Returns `SubmitEmailError::ReservationIncomplete` when the idempotency key matches a
    /// submission that never completed.
    /// Returns `SubmitEmailError::AllRecipientsSuppressed` when every recipient is suppressed.
    #[allow(clippy::too_many_lines)]
    #[tracing::instrument(skip_all, name = "submit_email", fields(email_id = tracing::field::Empty, correlation_id = tracing::field::Empty))]
    pub async fn execute(&self, input: SubmitEmailInput) -> Result<EmailId, SubmitEmailError> {
        self.check_sender_allowed(&input.sender, input.sender_domains.as_ref())?;
        let id = EmailId::default();
        if let Some(ref cid) = input.correlation_id {
            tracing::Span::current().record("correlation_id", cid.as_str());
        }

        // Reserve the row with an empty attachment list first; the final list is
        // patched in after blobs are written so the worker never sees stale refs.
        let keys: Vec<String> = input
            .recipients
            .iter()
            .map(|(_, address)| suppression_key(address))
            .collect();
        let suppressed = self.suppressions.find_suppressed(&keys).await?;
        let filtered: Vec<_> = input
            .recipients
            .iter()
            .filter(|(_, address)| !suppressed.contains(&suppression_key(address)))
            .cloned()
            .collect();
        let all_suppressed = filtered.is_empty();
        let mut dropped: Vec<String> = keys
            .into_iter()
            .filter(|key| suppressed.contains(key))
            .collect();
        dropped.sort();
        dropped.dedup();
        // The stored envelope is the published one, so a recovery replay never resurrects a
        // dropped recipient. A fully suppressed row only reserves the key until it is deleted.
        let recipients = if all_suppressed {
            input.recipients.clone()
        } else {
            filtered
        };

        let envelope_for_reservation = Envelope {
            owner: input.owner.clone(),
            idempotency_key: input.idempotency_key.clone(),
            correlation_id: input.correlation_id.clone(),
            subject: input.subject.clone(),
            sender: input.sender.clone(),
            recipients: recipients.clone(),
            body: input.body.clone(),
            variables: input.variables.clone(),
            attachments: vec![],
            reply_to: input.reply_to.clone(),
            headers: input.headers.clone(),
            unsubscribe: input.unsubscribe.clone(),
            tags: input.tags.clone(),
            metadata: input.metadata.clone(),
            send_at_ms: None,
        };

        let result = self.repository.save(id, &envelope_for_reservation).await?;
        match result {
            SaveResult::Duplicate {
                id: existing_id,
                state,
                recovery,
            } => {
                // Record the persisted id, not the freshly generated one that is
                // discarded on an idempotent resubmit.
                tracing::Span::current().record("email_id", existing_id.as_uuid().to_string());
                return self
                    .resolve_duplicate(existing_id, state, recovery, input.sender_domains.as_ref())
                    .await;
            }
            SaveResult::Created(_) => {
                tracing::Span::current().record("email_id", id.as_uuid().to_string());
            }
        }
        if all_suppressed {
            return Err(self
                .rollback(id, &[], SubmitEmailError::AllRecipientsSuppressed)
                .await);
        }

        let SubmitEmailInput {
            owner,
            sender_domains: _,
            idempotency_key,
            correlation_id,
            subject,
            sender,
            recipients: _,
            body,
            variables,
            attachments,
            reply_to,
            headers,
            unsubscribe,
            tags,
            metadata,
        } = input;

        let mut written_refs: Vec<AttachmentRef> = Vec::with_capacity(attachments.len());
        for att in attachments {
            let (filename, content_type, reader, content_id) = match att {
                AttachmentInput::Inline {
                    filename,
                    content_type,
                    bytes,
                    content_id,
                } => (
                    filename,
                    content_type,
                    Box::pin(std::io::Cursor::new(bytes)) as AttachmentReader,
                    content_id,
                ),
                AttachmentInput::Stream {
                    filename,
                    content_type,
                    reader,
                    content_id,
                } => (filename, content_type, reader, content_id),
                AttachmentInput::Remote {
                    filename,
                    content_type,
                    url,
                    content_id,
                } => match self.attachment_fetcher.fetch(&url).await {
                    Ok(r) => (filename, content_type, r, content_id),
                    Err(fetch_err) => {
                        let original = SubmitEmailError::AttachmentFetch {
                            source: anyhow::Error::new(fetch_err),
                        };
                        return Err(self.rollback(id, &written_refs, original).await);
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
                        content_id,
                    });
                }
                Err(store_err) => {
                    let original = SubmitEmailError::AttachmentStore {
                        source: anyhow::Error::new(store_err),
                    };
                    return Err(self.rollback(id, &written_refs, original).await);
                }
            }
        }

        if estimate_message_bytes(&written_refs, &body) > self.limits.max_message_bytes {
            let original = SubmitEmailError::MessageTooLarge {
                max_bytes: self.limits.max_message_bytes,
            };
            return Err(self.rollback(id, &written_refs, original).await);
        }

        if !written_refs.is_empty()
            && let Err(e) = self.repository.set_attachments(id, &written_refs).await
        {
            return Err(self
                .rollback(id, &written_refs, SubmitEmailError::Persist(e))
                .await);
        }

        let envelope = Envelope {
            owner,
            idempotency_key,
            correlation_id,
            subject,
            sender,
            recipients,
            body,
            variables,
            attachments: written_refs,
            reply_to,
            headers,
            unsubscribe,
            tags,
            metadata,
            send_at_ms: None,
        };

        if let Err(e) = self
            .repository
            .set_enqueue_state(id, &envelope.owner, EnqueueState::Publishable)
            .await
        {
            // A failed write may still have committed, so a concurrent same-key recovery can
            // already be publishing: a keyed row is never deleted once this transition is attempted.
            let original = SubmitEmailError::Persist(e);
            if envelope.idempotency_key.is_some() {
                return Err(original);
            }
            return Err(self.rollback(id, &envelope.attachments, original).await);
        }

        match self.queue.enqueue(id, &envelope).await {
            Ok(outcome) => {
                if outcome == EnqueueOutcome::Inserted {
                    self.mark_enqueued_and_announce(id, &envelope).await;
                }
                self.announce_suppressed(id, &envelope, dropped).await;
            }
            // The message may be queued: keep the row and blobs so a retry can republish it.
            Err(e @ EmailQueueError::Uncertain { .. }) => {
                return Err(SubmitEmailError::Enqueue(e));
            }
            // Definitely not queued. A keyed row stays publishable for a retry, since a
            // concurrent same-key recovery may already be publishing it.
            Err(e @ EmailQueueError::Storage { .. }) => {
                let original = SubmitEmailError::Enqueue(e);
                if envelope.idempotency_key.is_some() {
                    return Err(original);
                }
                return Err(self.rollback(id, &envelope.attachments, original).await);
            }
        }
        Ok(id)
    }
}

impl<R, Q, P, A, F, X> SubmitEmailUseCase for SubmitEmailService<R, Q, P, A, F, X>
where
    R: EmailRepository + Send + Sync + 'static,
    Q: EmailQueue + Send + Sync + 'static,
    P: EventPublisher + Send + Sync + 'static,
    A: AttachmentStore + Send + Sync + 'static,
    F: AttachmentFetcher + Send + Sync + 'static,
    X: SuppressionRepository,
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

    use super::{normalize_sender_domain, normalize_sender_domains};

    use crate::entity::attachment::{AttachmentRef, BlobRef};
    use crate::entity::body::Plain;
    use crate::entity::email::{EmailId, RecipientKind};
    use crate::entity::envelope::Envelope;
    use crate::entity::lifecycle_event::{EventContext, LifecycleEvent};
    use crate::port::attachment_fetcher::{AttachmentFetchError, AttachmentFetcher};
    use crate::port::attachment_store::{
        AttachmentReader, AttachmentStore, AttachmentStoreError, PutResult,
    };
    use crate::port::email_queue::{EmailQueue, EmailQueueError, EnqueueOutcome};
    use crate::port::email_repository::{
        DuplicateState, EmailRepository, EmailRepositoryError, EnqueueState, SaveResult,
    };
    use crate::port::event_publisher::{EventPublisher, EventPublisherError};

    use super::{
        AttachmentInput, SubmitEmailError, SubmitEmailInput, SubmitEmailService, SubmitLimits,
    };

    fn make_input(sender: &str) -> SubmitEmailInput {
        SubmitEmailInput {
            owner: String::new(),
            sender_domains: None,
            idempotency_key: None,
            correlation_id: None,
            subject: None,
            sender: sender.into(),
            recipients: vec![(RecipientKind::To, "to@example.com".into())],
            body: crate::entity::body::BodySource::Plain(
                Plain::try_new(Some("hello".into()), None).unwrap(),
            ),
            variables: serde_json::Map::new(),
            attachments: vec![],
            reply_to: vec![],
            headers: vec![],
            unsubscribe: None,
            tags: vec![],
            metadata: std::collections::BTreeMap::new(),
        }
    }

    #[derive(Clone)]
    struct FakeRepository {
        saved: Arc<Mutex<Vec<(EmailId, String)>>>,
        deleted: Arc<Mutex<Vec<EmailId>>>,
    }

    impl FakeRepository {
        fn new() -> Self {
            Self {
                saved: Arc::new(Mutex::new(Vec::new())),
                deleted: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn live_count(&self) -> usize {
            let saved = self.saved.lock().unwrap();
            let deleted = self.deleted.lock().unwrap();
            saved.iter().filter(|(id, _)| !deleted.contains(id)).count()
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

        async fn delete(&self, id: EmailId) -> Result<(), EmailRepositoryError> {
            self.deleted.lock().unwrap().push(id);
            Ok(())
        }

        async fn set_enqueue_state(
            &self,
            _id: EmailId,
            _owner: &str,
            _state: EnqueueState,
        ) -> Result<(), EmailRepositoryError> {
            Ok(())
        }

        async fn list_all_attachment_blobs(
            &self,
        ) -> Result<Vec<crate::entity::attachment::BlobRef>, EmailRepositoryError> {
            Ok(vec![])
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

        async fn set_enqueue_state(
            &self,
            _id: EmailId,
            _owner: &str,
            _state: EnqueueState,
        ) -> Result<(), EmailRepositoryError> {
            Ok(())
        }

        async fn list_all_attachment_blobs(
            &self,
        ) -> Result<Vec<crate::entity::attachment::BlobRef>, EmailRepositoryError> {
            Ok(vec![])
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
            Ok(SaveResult::Duplicate {
                id: self.existing_id,
                state: DuplicateState::Complete,
                recovery: None,
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
            Ok(())
        }

        async fn delete(&self, _id: EmailId) -> Result<(), EmailRepositoryError> {
            Ok(())
        }

        async fn set_enqueue_state(
            &self,
            _id: EmailId,
            _owner: &str,
            _state: EnqueueState,
        ) -> Result<(), EmailRepositoryError> {
            Ok(())
        }

        async fn list_all_attachment_blobs(
            &self,
        ) -> Result<Vec<crate::entity::attachment::BlobRef>, EmailRepositoryError> {
            Ok(vec![])
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
        async fn enqueue(
            &self,
            id: EmailId,
            _envelope: &Envelope,
        ) -> Result<EnqueueOutcome, EmailQueueError> {
            self.enqueued.lock().unwrap().push(id);
            Ok(EnqueueOutcome::Inserted)
        }

        async fn dequeue(
            &self,
        ) -> Result<crate::port::email_queue::DequeuedEmail, EmailQueueError> {
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
        async fn enqueue(
            &self,
            _id: EmailId,
            _envelope: &Envelope,
        ) -> Result<EnqueueOutcome, EmailQueueError> {
            Err(EmailQueueError::Storage {
                source: anyhow::anyhow!("queue down"),
            })
        }

        async fn dequeue(
            &self,
        ) -> Result<crate::port::email_queue::DequeuedEmail, EmailQueueError> {
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
        delete_count: Arc<Mutex<usize>>,
    }

    impl FakeAttachmentStore {
        fn new() -> Self {
            Self {
                put_count: Arc::new(Mutex::new(0)),
                delete_count: Arc::new(Mutex::new(0)),
            }
        }

        fn outstanding_blobs(&self) -> usize {
            let puts = *self.put_count.lock().unwrap();
            let deletes = *self.delete_count.lock().unwrap();
            puts.saturating_sub(deletes)
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
            *self.delete_count.lock().unwrap() += 1;
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

    async fn submit_with_allowed(
        allowed: Option<&[&str]>,
        sender: &str,
    ) -> (Result<EmailId, SubmitEmailError>, usize, usize) {
        let repo = FakeRepository::new();
        let queue = FakeQueue::new();
        let service = SubmitEmailService::new(
            repo.clone(),
            queue.clone(),
            FakeEventPublisher::new(),
            FakeAttachmentStore::new(),
            FakeFetcher,
        )
        .with_limits(SubmitLimits {
            allowed_sender_domains: allowed.map(|d| normalize_sender_domains(d).unwrap()),
            ..SubmitLimits::default()
        });
        let result = service.execute(make_input(sender)).await;
        let saved = repo.saved.lock().unwrap().len();
        let enqueued = queue.enqueued.lock().unwrap().len();
        (result, saved, enqueued)
    }

    #[tokio::test]
    async fn disallowed_sender_domain_saves_and_enqueues_nothing() {
        let (result, saved, enqueued) =
            submit_with_allowed(Some(&["example.com"]), "a@other.com").await;
        let err = result.unwrap_err();
        assert!(matches!(
            err,
            SubmitEmailError::SenderDomainNotAllowed { ref domain } if domain == "other.com"
        ));
        assert!(!err.is_transient());
        assert_eq!((saved, enqueued), (0, 0));
    }

    #[tokio::test]
    async fn empty_allow_list_denies_every_sender_and_none_allows_all() {
        let (denied, ..) = submit_with_allowed(Some(&[]), "a@example.com").await;
        assert!(matches!(
            denied,
            Err(SubmitEmailError::SenderDomainNotAllowed { .. })
        ));
        let (allowed, saved, _) = submit_with_allowed(None, "a@anything.org").await;
        assert!(allowed.is_ok());
        assert_eq!(saved, 1);
    }

    #[tokio::test]
    async fn sender_domain_matching_is_exact_case_insensitive_and_idn_aware() {
        for (entries, sender) in [
            (vec!["Example.COM."], "a@example.com"),
            (vec!["example.com"], "Name <a@EXAMPLE.com>"),
            (vec!["bücher.de"], "a@xn--bcher-kva.de"),
            (vec!["xn--bcher-kva.de"], "a@Bücher.de"),
        ] {
            let (result, ..) = submit_with_allowed(Some(&entries), sender).await;
            assert!(result.is_ok(), "{entries:?} vs {sender}");
        }
        for (entries, sender) in [
            (vec!["example.com"], "a@news.example.com"),
            (vec!["example.com"], "a@badexample.com"),
        ] {
            let (result, ..) = submit_with_allowed(Some(&entries), sender).await;
            assert!(result.is_err(), "{entries:?} vs {sender}");
        }
    }

    #[test]
    fn normalize_sender_domain_enforces_hostname_rules() {
        let long_label = format!("{}.com", "a".repeat(64));
        for bad in [
            "-example.com",
            "example-.com",
            long_label.as_str(),
            "example.com..",
            "a..com",
            "",
        ] {
            assert_eq!(normalize_sender_domain(bad), None, "{bad:?}");
        }
        for ok in ["xn--bcher-kva.de", "a-b.example.com", "Example.com."] {
            assert!(normalize_sender_domain(ok).is_some(), "{ok}");
        }
        let too_long = format!("{}.com", vec!["a".repeat(60); 5].join("."));
        assert_eq!(normalize_sender_domain(&too_long), None);
    }

    #[tokio::test]
    async fn recovery_republish_rechecks_the_stored_sender_against_the_policy() {
        let repo = KeyedRepository::new();
        let store = FakeAttachmentStore::new();
        let mut first = keyed_input("k", 10);
        first.sender = "a@removed.example".to_owned();
        let failing = service_with(
            &repo,
            &ScriptedQueue::failing(&[false]),
            &FakeEventPublisher::new(),
            &store,
        );
        failing.execute(first).await.unwrap_err();
        assert_eq!(repo.states(), vec![RowState::Publishable]);

        let queue = ScriptedQueue::failing(&[]);
        let restricted = service_with(&repo, &queue, &FakeEventPublisher::new(), &store)
            .with_limits(SubmitLimits {
                allowed_sender_domains: Some(
                    normalize_sender_domains(["allowed.example"]).unwrap(),
                ),
                ..SubmitLimits::default()
            });
        let mut retry = keyed_input("k", 10);
        retry.sender = "a@allowed.example".to_owned();

        let err = restricted.execute(retry).await.unwrap_err();

        assert!(matches!(
            err,
            SubmitEmailError::SenderDomainNotAllowed { ref domain } if domain == "removed.example"
        ));
        assert!(queue.calls.lock().unwrap().is_empty(), "nothing enqueued");
        assert_eq!(repo.states(), vec![RowState::Publishable], "row kept");
    }

    #[test]
    fn invalid_allowed_domain_entry_is_an_error() {
        assert!(normalize_sender_domains(["ok.com", "exa mple\u{0}.com"]).is_err());
        assert!(normalize_sender_domains([""]).is_err());
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
    async fn enqueue_failure_compensates_and_propagates_error() {
        let repo = FakeRepository::new();
        let store = FakeAttachmentStore::new();
        let spy = FakeEventPublisher::new();
        let mut input = make_input("sender@example.com");
        input.attachments.push(AttachmentInput::Inline {
            content_id: None,
            filename: "attach.txt".into(),
            content_type: "text/plain".into(),
            bytes: bytes::Bytes::from_static(b"data"),
        });
        let service = SubmitEmailService::new(
            repo.clone(),
            FailingQueue,
            spy.clone(),
            store.clone(),
            FakeFetcher,
        );
        let err = service.execute(input).await.unwrap_err();
        assert!(matches!(err, SubmitEmailError::Enqueue(_)));
        // Row must have been deleted as compensation.
        assert_eq!(
            repo.live_count(),
            0,
            "row must be deleted after enqueue failure"
        );
        // All blobs must have been deleted as compensation.
        assert_eq!(
            store.outstanding_blobs(),
            0,
            "blobs must be deleted after enqueue failure"
        );
        // No Queued event must have been published.
        assert!(
            spy.published.lock().unwrap().is_empty(),
            "no event must be published when enqueue fails"
        );
    }

    fn input_with_attachment(size: usize) -> SubmitEmailInput {
        let mut input = make_input("sender@example.com");
        input.attachments.push(AttachmentInput::Inline {
            content_id: None,
            filename: "big.bin".into(),
            content_type: "application/octet-stream".into(),
            bytes: bytes::Bytes::from(vec![0u8; size]),
        });
        input
    }

    #[tokio::test]
    async fn estimate_over_limit_rejects_cleans_up_and_does_not_enqueue() {
        let repo = FakeRepository::new();
        let queue = FakeQueue::new();
        let store = FakeAttachmentStore::new();
        let spy = FakeEventPublisher::new();
        let service = SubmitEmailService::new(
            repo.clone(),
            queue.clone(),
            spy.clone(),
            store.clone(),
            FakeFetcher,
        )
        .with_limits(SubmitLimits {
            max_message_bytes: 100,
            ..SubmitLimits::default()
        });

        let err = service
            .execute(input_with_attachment(100))
            .await
            .unwrap_err();

        assert!(matches!(
            err,
            SubmitEmailError::MessageTooLarge { max_bytes: 100 }
        ));
        assert!(!err.is_transient());
        assert_eq!(repo.live_count(), 0, "row must be deleted");
        assert_eq!(store.outstanding_blobs(), 0, "blobs must be deleted");
        assert!(queue.enqueued.lock().unwrap().is_empty());
        assert!(spy.published.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn estimate_under_limit_is_accepted() {
        let queue = FakeQueue::new();
        let service = SubmitEmailService::new(
            FakeRepository::new(),
            queue.clone(),
            FakeEventPublisher::new(),
            FakeAttachmentStore::new(),
            FakeFetcher,
        )
        .with_limits(SubmitLimits {
            max_message_bytes: 100,
            ..SubmitLimits::default()
        });

        service.execute(input_with_attachment(50)).await.unwrap();

        assert_eq!(queue.enqueued.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn huge_variable_is_not_part_of_the_submit_estimate() {
        // Variable growth is only caught by the final wire-size check in the SMTP adapter.
        let queue = FakeQueue::new();
        let service = SubmitEmailService::new(
            FakeRepository::new(),
            queue.clone(),
            FakeEventPublisher::new(),
            FakeAttachmentStore::new(),
            FakeFetcher,
        )
        .with_limits(SubmitLimits {
            max_message_bytes: 100,
            ..SubmitLimits::default()
        });
        let mut input = make_input("sender@example.com");
        input
            .variables
            .insert("huge".into(), serde_json::Value::String("x".repeat(10_000)));

        service.execute(input).await.unwrap();

        assert_eq!(queue.enqueued.lock().unwrap().len(), 1);
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum RowState {
        Reserved,
        Publishable,
        Enqueued,
    }

    struct KeyedRow {
        id: EmailId,
        key: Option<String>,
        state: RowState,
        envelope: Envelope,
    }

    type KeyedRows = Arc<Mutex<Vec<KeyedRow>>>;

    /// Repository with real idempotency-key and enqueue-state semantics and switchable failures.
    #[derive(Clone)]
    struct KeyedRepository {
        rows: KeyedRows,
        fail_delete: Arc<std::sync::atomic::AtomicBool>,
        fail_publishable: Arc<std::sync::atomic::AtomicBool>,
        /// Applies the `publishable` state, then reports failure.
        fail_publishable_after_commit: Arc<std::sync::atomic::AtomicBool>,
        fail_enqueued: Arc<std::sync::atomic::AtomicBool>,
    }

    impl KeyedRepository {
        fn new() -> Self {
            Self {
                rows: Arc::new(Mutex::new(Vec::new())),
                fail_delete: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                fail_publishable: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                fail_publishable_after_commit: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                fail_enqueued: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            }
        }

        fn states(&self) -> Vec<RowState> {
            self.rows.lock().unwrap().iter().map(|r| r.state).collect()
        }
    }

    #[allow(async_fn_in_trait)]
    impl EmailRepository for KeyedRepository {
        async fn save(
            &self,
            id: EmailId,
            envelope: &Envelope,
        ) -> Result<SaveResult, EmailRepositoryError> {
            let mut rows = self.rows.lock().unwrap();
            if let Some(key) = &envelope.idempotency_key
                && let Some(row) = rows
                    .iter()
                    .find(|r| r.key.as_ref() == Some(key) && r.envelope.owner == envelope.owner)
            {
                return Ok(SaveResult::Duplicate {
                    id: row.id,
                    state: if row.state == RowState::Reserved {
                        DuplicateState::Reserved
                    } else {
                        DuplicateState::Complete
                    },
                    recovery: (row.state == RowState::Publishable).then(|| row.envelope.clone()),
                });
            }
            rows.push(KeyedRow {
                id,
                key: envelope.idempotency_key.clone(),
                state: RowState::Reserved,
                envelope: envelope.clone(),
            });
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
            id: EmailId,
            attachments: &[AttachmentRef],
        ) -> Result<(), EmailRepositoryError> {
            let mut rows = self.rows.lock().unwrap();
            if let Some(row) = rows.iter_mut().find(|r| r.id == id) {
                row.envelope.attachments = attachments.to_vec();
            }
            Ok(())
        }

        async fn set_enqueue_state(
            &self,
            id: EmailId,
            owner: &str,
            state: EnqueueState,
        ) -> Result<(), EmailRepositoryError> {
            use std::sync::atomic::Ordering::SeqCst;
            let (fail, new_state) = match state {
                EnqueueState::Publishable => (&self.fail_publishable, RowState::Publishable),
                EnqueueState::Enqueued => (&self.fail_enqueued, RowState::Enqueued),
            };
            if fail.load(SeqCst) {
                return Err(EmailRepositoryError::Storage {
                    source: anyhow::anyhow!("state write failed"),
                });
            }
            let mut rows = self.rows.lock().unwrap();
            if let Some(row) = rows
                .iter_mut()
                .find(|r| r.id == id && r.envelope.owner == owner)
            {
                row.state = new_state;
            }
            if state == EnqueueState::Publishable && self.fail_publishable_after_commit.load(SeqCst)
            {
                return Err(EmailRepositoryError::Storage {
                    source: anyhow::anyhow!("state write failed after commit"),
                });
            }
            Ok(())
        }

        async fn delete(&self, id: EmailId) -> Result<(), EmailRepositoryError> {
            if self.fail_delete.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(EmailRepositoryError::Storage {
                    source: anyhow::anyhow!("delete failed"),
                });
            }
            self.rows.lock().unwrap().retain(|r| r.id != id);
            Ok(())
        }

        async fn list_all_attachment_blobs(
            &self,
        ) -> Result<Vec<crate::entity::attachment::BlobRef>, EmailRepositoryError> {
            Ok(vec![])
        }
    }

    type QueueCalls = Arc<Mutex<Vec<(EmailId, Vec<AttachmentRef>)>>>;

    /// Queue that records every call and fails the first calls as scripted.
    #[derive(Clone)]
    struct ScriptedQueue {
        calls: QueueCalls,
        script: Arc<Mutex<std::collections::VecDeque<bool>>>,
        already_enqueued: Arc<std::sync::atomic::AtomicBool>,
    }

    impl ScriptedQueue {
        /// Each entry fails one call: `true` is `Uncertain`, `false` is `Storage`.
        fn failing(script: &[bool]) -> Self {
            Self {
                calls: Arc::new(Mutex::new(Vec::new())),
                script: Arc::new(Mutex::new(script.iter().copied().collect())),
                already_enqueued: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            }
        }
    }

    #[allow(async_fn_in_trait)]
    impl EmailQueue for ScriptedQueue {
        async fn enqueue(
            &self,
            id: EmailId,
            envelope: &Envelope,
        ) -> Result<EnqueueOutcome, EmailQueueError> {
            self.calls
                .lock()
                .unwrap()
                .push((id, envelope.attachments.clone()));
            match self.script.lock().unwrap().pop_front() {
                None if self
                    .already_enqueued
                    .load(std::sync::atomic::Ordering::SeqCst) =>
                {
                    Ok(EnqueueOutcome::AlreadyEnqueued)
                }
                None => Ok(EnqueueOutcome::Inserted),
                Some(true) => Err(EmailQueueError::Uncertain {
                    source: anyhow::anyhow!("ack lost"),
                }),
                Some(false) => Err(EmailQueueError::Storage {
                    source: anyhow::anyhow!("rejected"),
                }),
            }
        }

        async fn dequeue(
            &self,
        ) -> Result<crate::port::email_queue::DequeuedEmail, EmailQueueError> {
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

    fn service_with(
        repo: &KeyedRepository,
        queue: &ScriptedQueue,
        events: &FakeEventPublisher,
        store: &FakeAttachmentStore,
    ) -> SubmitEmailService<
        KeyedRepository,
        ScriptedQueue,
        FakeEventPublisher,
        FakeAttachmentStore,
        FakeFetcher,
    > {
        SubmitEmailService::new(
            repo.clone(),
            queue.clone(),
            events.clone(),
            store.clone(),
            FakeFetcher,
        )
    }

    fn queued_count(events: &FakeEventPublisher) -> usize {
        events
            .published
            .lock()
            .unwrap()
            .iter()
            .filter(|e| matches!(e, LifecycleEvent::Queued { .. }))
            .count()
    }

    fn keyed_input(key: &str, attachment_size: usize) -> SubmitEmailInput {
        let mut input = input_with_attachment(attachment_size);
        input.idempotency_key = Some(key.to_owned());
        input
    }

    #[tokio::test]
    async fn failed_row_delete_replaces_message_too_large_with_a_transient_error() {
        let repo = KeyedRepository::new();
        repo.fail_delete
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let queue = FakeQueue::new();
        let service = SubmitEmailService::new(
            repo.clone(),
            queue.clone(),
            FakeEventPublisher::new(),
            FakeAttachmentStore::new(),
            FakeFetcher,
        )
        .with_limits(SubmitLimits {
            max_message_bytes: 100,
            ..SubmitLimits::default()
        });

        let err = service.execute(keyed_input("k", 100)).await.unwrap_err();

        assert!(matches!(err, SubmitEmailError::Persist(_)), "{err}");
        assert!(err.is_transient());
        assert!(queue.enqueued.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn successful_cleanup_lets_a_same_key_retry_be_accepted_as_new() {
        let repo = KeyedRepository::new();
        let queue = FakeQueue::new();
        let service = SubmitEmailService::new(
            repo,
            queue.clone(),
            FakeEventPublisher::new(),
            FakeAttachmentStore::new(),
            FakeFetcher,
        )
        .with_limits(SubmitLimits {
            max_message_bytes: 100,
            ..SubmitLimits::default()
        });

        let err = service.execute(keyed_input("k", 100)).await.unwrap_err();
        assert!(matches!(err, SubmitEmailError::MessageTooLarge { .. }));
        let id = service.execute(keyed_input("k", 10)).await.unwrap();

        let enqueued = queue.enqueued.lock().unwrap();
        assert_eq!(enqueued.len(), 1);
        assert_eq!(enqueued[0], id, "retry must be a new row, not a Duplicate");
    }

    #[tokio::test]
    async fn failed_row_delete_after_fetch_failure_is_also_transient() {
        let repo = KeyedRepository::new();
        repo.fail_delete
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let service = SubmitEmailService::new(
            repo,
            FakeQueue::new(),
            FakeEventPublisher::new(),
            FakeAttachmentStore::new(),
            FailingFetcher,
        );
        let mut input = make_input("sender@example.com");
        input.attachments.push(AttachmentInput::Remote {
            content_id: None,
            filename: "r.bin".into(),
            content_type: "application/octet-stream".into(),
            url: url::Url::parse("https://example.com/r.bin").unwrap(),
        });

        let err = service.execute(input).await.unwrap_err();

        assert!(matches!(err, SubmitEmailError::Persist(_)), "{err}");
        assert!(err.is_transient());
    }

    #[tokio::test]
    async fn uncertain_keeps_row_and_blobs_and_retry_reenqueues_once_announced() {
        let repo = KeyedRepository::new();
        let queue = ScriptedQueue::failing(&[true]);
        let events = FakeEventPublisher::new();
        let store = FakeAttachmentStore::new();
        let service = service_with(&repo, &queue, &events, &store);

        let err = service.execute(keyed_input("k", 10)).await.unwrap_err();

        assert!(matches!(
            err,
            SubmitEmailError::Enqueue(EmailQueueError::Uncertain { .. })
        ));
        assert!(err.is_transient());
        assert_eq!(repo.states(), vec![RowState::Publishable]);
        assert_eq!(store.outstanding_blobs(), 1);
        assert_eq!(queued_count(&events), 0);

        let id = service.execute(keyed_input("k", 10)).await.unwrap();

        let calls = queue.calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].0, id);
        assert_eq!(calls[1].0, id, "the retry republishes the same id");
        assert_eq!(
            calls[0].1, calls[1].1,
            "the retry republishes the stored envelope"
        );
        assert_eq!(queued_count(&events), 1);
        assert_eq!(repo.states(), vec![RowState::Enqueued]);
        assert_eq!(store.outstanding_blobs(), 1);
    }

    #[tokio::test]
    async fn already_enqueued_recovery_skips_state_write_and_queued_event() {
        let repo = KeyedRepository::new();
        let queue = ScriptedQueue::failing(&[true]);
        let events = FakeEventPublisher::new();
        let store = FakeAttachmentStore::new();
        let service = service_with(&repo, &queue, &events, &store);
        service.execute(keyed_input("k", 10)).await.unwrap_err();
        queue
            .already_enqueued
            .store(true, std::sync::atomic::Ordering::SeqCst);

        let id = service.execute(keyed_input("k", 10)).await.unwrap();

        assert_eq!(queue.calls.lock().unwrap().len(), 2);
        assert_eq!(queue.calls.lock().unwrap()[1].0, id);
        assert_eq!(repo.states(), vec![RowState::Publishable]);
        assert_eq!(queued_count(&events), 0);
    }

    #[tokio::test]
    async fn already_enqueued_submit_returns_the_id_without_announcing() {
        let repo = KeyedRepository::new();
        let queue = ScriptedQueue::failing(&[]);
        queue
            .already_enqueued
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let events = FakeEventPublisher::new();
        let store = FakeAttachmentStore::new();
        let service = service_with(&repo, &queue, &events, &store);

        service.execute(keyed_input("k", 10)).await.unwrap();

        assert_eq!(repo.states(), vec![RowState::Publishable]);
        assert_eq!(queued_count(&events), 0);
    }

    #[tokio::test]
    async fn failed_post_publish_state_write_still_succeeds_and_retry_republishes() {
        let repo = KeyedRepository::new();
        repo.fail_enqueued
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let queue = ScriptedQueue::failing(&[]);
        let service = service_with(
            &repo,
            &queue,
            &FakeEventPublisher::new(),
            &FakeAttachmentStore::new(),
        );

        let id = service.execute(keyed_input("k", 10)).await.unwrap();
        assert_eq!(repo.states(), vec![RowState::Publishable]);

        let again = service.execute(keyed_input("k", 10)).await.unwrap();

        assert_eq!(again, id);
        let calls = queue.calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].0, calls[1].0);
    }

    #[tokio::test]
    async fn definite_rejection_keeps_a_keyed_row_publishable() {
        let repo = KeyedRepository::new();
        let store = FakeAttachmentStore::new();
        let service = service_with(
            &repo,
            &ScriptedQueue::failing(&[false]),
            &FakeEventPublisher::new(),
            &store,
        );

        let err = service.execute(keyed_input("k", 10)).await.unwrap_err();

        assert!(matches!(
            err,
            SubmitEmailError::Enqueue(EmailQueueError::Storage { .. })
        ));
        assert_eq!(repo.states(), vec![RowState::Publishable]);
        assert_eq!(store.outstanding_blobs(), 1);
    }

    #[tokio::test]
    async fn definite_rejection_cleans_an_unkeyed_row() {
        let repo = KeyedRepository::new();
        let store = FakeAttachmentStore::new();
        let service = service_with(
            &repo,
            &ScriptedQueue::failing(&[false]),
            &FakeEventPublisher::new(),
            &store,
        );

        service
            .execute(input_with_attachment(10))
            .await
            .unwrap_err();

        assert!(repo.states().is_empty());
        assert_eq!(store.outstanding_blobs(), 0);
    }

    #[tokio::test]
    async fn unkeyed_publishable_write_failure_cleans_up_and_never_enqueues() {
        let repo = KeyedRepository::new();
        repo.fail_publishable
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let queue = ScriptedQueue::failing(&[]);
        let store = FakeAttachmentStore::new();
        let service = service_with(&repo, &queue, &FakeEventPublisher::new(), &store);

        let err = service
            .execute(input_with_attachment(10))
            .await
            .unwrap_err();

        assert!(matches!(err, SubmitEmailError::Persist(_)));
        assert!(queue.calls.lock().unwrap().is_empty());
        assert!(repo.states().is_empty());
        assert_eq!(store.outstanding_blobs(), 0);
    }

    #[tokio::test]
    async fn keyed_publishable_write_failure_deletes_nothing() {
        let repo = KeyedRepository::new();
        repo.fail_publishable
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let queue = ScriptedQueue::failing(&[]);
        let store = FakeAttachmentStore::new();
        let service = service_with(&repo, &queue, &FakeEventPublisher::new(), &store);

        let err = service.execute(keyed_input("k", 10)).await.unwrap_err();

        assert!(matches!(err, SubmitEmailError::Persist(_)));
        assert!(err.is_transient());
        assert!(queue.calls.lock().unwrap().is_empty());
        assert_eq!(repo.states(), vec![RowState::Reserved]);
        assert_eq!(store.outstanding_blobs(), 1);
    }

    #[tokio::test]
    async fn keyed_publishable_write_that_fails_after_commit_is_recovered_by_a_retry() {
        let repo = KeyedRepository::new();
        repo.fail_publishable_after_commit
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let queue = ScriptedQueue::failing(&[]);
        let store = FakeAttachmentStore::new();
        let events = FakeEventPublisher::new();
        let service = service_with(&repo, &queue, &events, &store);

        service.execute(keyed_input("k", 10)).await.unwrap_err();
        assert_eq!(repo.states(), vec![RowState::Publishable]);
        assert_eq!(store.outstanding_blobs(), 1);

        let id = service.execute(keyed_input("k", 10)).await.unwrap();

        let calls = queue.calls.lock().unwrap();
        assert_eq!(calls.len(), 1, "enqueued once");
        assert_eq!(calls[0].0, id);
        assert_eq!(calls[0].1.len(), 1, "the blob ref survives");
        assert_eq!(store.outstanding_blobs(), 1);
        assert_eq!(repo.states(), vec![RowState::Enqueued]);
    }

    #[tokio::test]
    async fn same_idempotency_key_from_another_owner_never_replays_a_publishable_row() {
        let repo = KeyedRepository::new();
        repo.fail_publishable_after_commit
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let queue = ScriptedQueue::failing(&[]);
        let events = FakeEventPublisher::new();
        let service = service_with(&repo, &queue, &events, &FakeAttachmentStore::new());
        let mut a = keyed_input("k", 10);
        a.owner = "a".into();
        service.execute(a).await.unwrap_err();
        assert_eq!(repo.states(), vec![RowState::Publishable]);
        repo.fail_publishable_after_commit
            .store(false, std::sync::atomic::Ordering::SeqCst);

        let mut b = keyed_input("k", 10);
        b.owner = "b".into();
        service.execute(b).await.unwrap();

        assert_eq!(
            repo.states(),
            vec![RowState::Publishable, RowState::Enqueued],
            "a's row is untouched and b got its own row"
        );
        assert_eq!(queue.calls.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn caller_sender_domains_are_intersected_with_the_global_list() {
        let run = |global: Option<&[&str]>, caller: Option<&[&str]>, sender: &str| {
            let mut input = make_input(sender);
            input.sender_domains = caller.map(|d| normalize_sender_domains(d).unwrap());
            let service = SubmitEmailService::new(
                FakeRepository::new(),
                FakeQueue::new(),
                FakeEventPublisher::new(),
                FakeAttachmentStore::new(),
                FakeFetcher,
            )
            .with_limits(SubmitLimits {
                allowed_sender_domains: global.map(|d| normalize_sender_domains(d).unwrap()),
                ..SubmitLimits::default()
            });
            async move { service.execute(input).await.is_ok() }
        };

        assert!(run(None, Some(&["a.com"]), "x@a.com").await);
        assert!(!run(None, Some(&["a.com"]), "x@b.com").await);
        assert!(!run(None, Some(&[]), "x@a.com").await);
        assert!(!run(Some(&["b.com"]), Some(&["a.com"]), "x@a.com").await);
        assert!(run(Some(&["a.com", "b.com"]), Some(&["a.com"]), "x@a.com").await);
        assert!(!run(Some(&["a.com"]), None, "x@b.com").await);
    }

    #[derive(Clone, Default)]
    struct FakeSuppressions(Arc<Mutex<std::collections::HashSet<String>>>);

    impl FakeSuppressions {
        fn with(addresses: &[&str]) -> Self {
            Self(Arc::new(Mutex::new(
                addresses.iter().map(|a| (*a).to_owned()).collect(),
            )))
        }
    }

    #[allow(async_fn_in_trait)]
    impl crate::port::suppression_repository::SuppressionRepository for FakeSuppressions {
        async fn list(
            &self,
            _params: crate::port::suppression_repository::ListSuppressionsParams,
        ) -> Result<
            Vec<crate::entity::suppression::Suppression>,
            crate::port::suppression_repository::SuppressionRepositoryError,
        > {
            Ok(vec![])
        }

        async fn add(
            &self,
            _entries: &[crate::entity::suppression::Suppression],
        ) -> Result<(), crate::port::suppression_repository::SuppressionRepositoryError> {
            Ok(())
        }

        async fn delete(
            &self,
            address: &str,
        ) -> Result<bool, crate::port::suppression_repository::SuppressionRepositoryError> {
            Ok(self.0.lock().unwrap().remove(address))
        }

        async fn find_suppressed(
            &self,
            addresses: &[String],
        ) -> Result<
            std::collections::HashSet<String>,
            crate::port::suppression_repository::SuppressionRepositoryError,
        > {
            let set = self.0.lock().unwrap();
            Ok(addresses
                .iter()
                .filter(|a| set.contains(*a))
                .cloned()
                .collect())
        }
    }

    fn two_recipient_input(key: &str) -> SubmitEmailInput {
        let mut input = keyed_input(key, 10);
        input.recipients = vec![
            (RecipientKind::To, "A@x.com".into()),
            (RecipientKind::Cc, "Bee <b@x.com>".into()),
        ];
        input
    }

    fn stored_recipients(repo: &KeyedRepository) -> Vec<Vec<String>> {
        repo.rows
            .lock()
            .unwrap()
            .iter()
            .map(|r| {
                r.envelope
                    .recipients
                    .iter()
                    .map(|(_, a)| a.clone())
                    .collect()
            })
            .collect()
    }

    #[tokio::test]
    async fn fully_suppressed_submits_leave_no_row_and_enqueue_nothing() {
        let repo = KeyedRepository::new();
        let queue = ScriptedQueue::failing(&[]);
        let suppressions = FakeSuppressions::with(&["a@x.com", "b@x.com"]);
        let service = service_with(
            &repo,
            &queue,
            &FakeEventPublisher::new(),
            &FakeAttachmentStore::new(),
        )
        .with_suppression_list(suppressions.clone());

        for _ in 0..2 {
            let err = service.execute(two_recipient_input("k")).await.unwrap_err();
            assert!(
                matches!(err, SubmitEmailError::AllRecipientsSuppressed),
                "{err}"
            );
            assert!(!err.is_transient());
        }
        assert!(repo.rows.lock().unwrap().is_empty());
        assert!(queue.calls.lock().unwrap().is_empty());

        suppressions.0.lock().unwrap().clear();
        service.execute(two_recipient_input("k")).await.unwrap();
        assert_eq!(queue.calls.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn every_event_of_a_submission_carries_its_tags_and_metadata() {
        let repo = KeyedRepository::new();
        let queue = ScriptedQueue::failing(&[]);
        let events = FakeEventPublisher::new();
        let service = service_with(&repo, &queue, &events, &FakeAttachmentStore::new())
            .with_suppression_list(FakeSuppressions::with(&["b@x.com"]));
        let mut input = two_recipient_input("k");
        input.tags = vec!["welcome".to_owned()];
        input.metadata.insert("order".to_owned(), "42".to_owned());

        service.execute(input).await.unwrap();

        let published = events.published.lock().unwrap();
        assert_eq!(published.len(), 2);
        for event in published.iter() {
            let payload = event.payload();
            assert_eq!(
                payload["tags"],
                serde_json::json!(["welcome"]),
                "{}",
                event.event_type()
            );
            assert_eq!(payload["metadata"], serde_json::json!({ "order": "42" }));
        }
        let stored = repo.rows.lock().unwrap();
        assert_eq!(stored[0].envelope.tags, vec!["welcome".to_owned()]);
    }

    #[tokio::test]
    async fn idn_equivalent_recipients_are_all_suppressed() {
        let repo = KeyedRepository::new();
        let queue = ScriptedQueue::failing(&[]);
        let service = service_with(
            &repo,
            &queue,
            &FakeEventPublisher::new(),
            &FakeAttachmentStore::new(),
        )
        .with_suppression_list(FakeSuppressions::with(&["u@xn--bcher-kva.example"]));
        let mut input = keyed_input("k", 10);
        input.recipients = [
            "u@bücher.example",
            "U@BÜCHER.example",
            "u@xn--bcher-kva.example",
        ]
        .map(|a| (RecipientKind::To, a.to_owned()))
        .to_vec();

        let err = service.execute(input).await.unwrap_err();

        assert!(
            matches!(err, SubmitEmailError::AllRecipientsSuppressed),
            "{err}"
        );
    }

    #[tokio::test]
    async fn suppression_added_after_acceptance_does_not_turn_a_resubmit_into_an_error() {
        let repo = KeyedRepository::new();
        let queue = ScriptedQueue::failing(&[]);
        let suppressions = FakeSuppressions::default();
        let service = service_with(
            &repo,
            &queue,
            &FakeEventPublisher::new(),
            &FakeAttachmentStore::new(),
        )
        .with_suppression_list(suppressions.clone());
        let id = service.execute(two_recipient_input("k")).await.unwrap();

        suppressions
            .0
            .lock()
            .unwrap()
            .extend(["a@x.com".to_owned(), "b@x.com".to_owned()]);
        let again = service.execute(two_recipient_input("k")).await.unwrap();

        assert_eq!(again, id);
        assert_eq!(queue.calls.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn partial_suppression_stores_and_publishes_only_the_remaining_recipients() {
        let repo = KeyedRepository::new();
        let queue = ScriptedQueue::failing(&[]);
        let events = FakeEventPublisher::new();
        let service = service_with(&repo, &queue, &events, &FakeAttachmentStore::new())
            .with_suppression_list(FakeSuppressions::with(&["b@x.com"]));

        service.execute(two_recipient_input("k")).await.unwrap();

        assert_eq!(stored_recipients(&repo), vec![vec!["A@x.com".to_owned()]]);
        let published = events.published.lock().unwrap();
        assert!(matches!(published[0], LifecycleEvent::Queued { .. }));
        assert!(matches!(
            &published[1],
            LifecycleEvent::Suppressed { addresses, .. } if addresses == &["b@x.com".to_owned()]
        ));
    }

    #[tokio::test]
    async fn recovery_replays_the_stored_filtered_envelope_even_after_the_suppression_is_removed() {
        let repo = KeyedRepository::new();
        let queue = ScriptedQueue::failing(&[true]);
        let suppressions = FakeSuppressions::with(&["b@x.com"]);
        let service = service_with(
            &repo,
            &queue,
            &FakeEventPublisher::new(),
            &FakeAttachmentStore::new(),
        )
        .with_suppression_list(suppressions.clone());
        service.execute(two_recipient_input("k")).await.unwrap_err();
        assert_eq!(repo.states(), vec![RowState::Publishable]);

        suppressions.0.lock().unwrap().clear();
        service.execute(two_recipient_input("k")).await.unwrap();

        assert_eq!(stored_recipients(&repo), vec![vec!["A@x.com".to_owned()]]);
        assert_eq!(
            queue.calls.lock().unwrap().len(),
            2,
            "uncertain call, then the replay"
        );
        assert_eq!(repo.states(), vec![RowState::Enqueued]);
    }

    #[tokio::test]
    async fn reserved_duplicate_is_incomplete_and_never_replayed() {
        let repo = KeyedRepository::new();
        repo.fail_delete
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let queue = ScriptedQueue::failing(&[]);
        let events = FakeEventPublisher::new();
        let service = service_with(&repo, &queue, &events, &FakeAttachmentStore::new())
            .with_limits(SubmitLimits {
                max_message_bytes: 100,
                ..SubmitLimits::default()
            });

        let err = service.execute(keyed_input("k", 100)).await.unwrap_err();
        assert!(matches!(err, SubmitEmailError::Persist(_)));
        assert_eq!(repo.states(), vec![RowState::Reserved]);

        let retry = service.execute(keyed_input("k", 10)).await.unwrap_err();

        assert!(
            matches!(retry, SubmitEmailError::ReservationIncomplete { .. }),
            "{retry}"
        );
        assert!(!retry.is_transient());
        assert!(queue.calls.lock().unwrap().is_empty());
        assert_eq!(queued_count(&events), 0);
    }

    #[test]
    fn estimate_saturates_instead_of_overflowing() {
        use crate::entity::attachment::BlobRef;
        let refs = vec![AttachmentRef {
            content_id: None,
            filename: "x".into(),
            content_type: "x/y".into(),
            size_bytes: u64::MAX,
            blob: BlobRef {
                backend: "t".into(),
                key: "k".into(),
            },
        }];
        let body =
            crate::entity::body::BodySource::Plain(Plain::try_new(Some("a".into()), None).unwrap());
        assert_eq!(super::estimate_message_bytes(&refs, &body), u64::MAX);
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
        assert_eq!(
            events[0],
            LifecycleEvent::Queued {
                id,
                context: EventContext::with_correlation(None),
            }
        );
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
            content_id: None,
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
            content_id: None,
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
            content_id: None,
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
            content_id: None,
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

    #[derive(Clone)]
    struct CapturingAttachmentStore {
        captured: Arc<Mutex<Vec<Vec<u8>>>>,
    }

    impl CapturingAttachmentStore {
        fn new() -> Self {
            Self {
                captured: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    #[allow(async_fn_in_trait)]
    impl AttachmentStore for CapturingAttachmentStore {
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
            self.captured.lock().unwrap().push(buf);
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

    #[tokio::test]
    async fn execute_writes_stream_attachment_via_attachment_store() {
        let repo = FakeRepository::new();
        let queue = FakeQueue::new();
        let store = CapturingAttachmentStore::new();
        let captured = store.captured.clone();
        let mut input = make_input("sender@example.com");
        input.attachments.push(AttachmentInput::Stream {
            content_id: None,
            filename: "streamed.txt".into(),
            content_type: "text/plain".into(),
            reader: Box::pin(std::io::Cursor::new(b"streamed bytes".to_vec())),
        });
        let service = SubmitEmailService::new(
            repo.clone(),
            queue.clone(),
            FakeEventPublisher::new(),
            store,
            FakeFetcher,
        );
        service.execute(input).await.unwrap();
        let blobs = captured.lock().unwrap();
        assert_eq!(blobs.len(), 1);
        assert_eq!(blobs[0], b"streamed bytes");
    }

    #[test]
    fn is_transient_persist_is_true() {
        let err = SubmitEmailError::Persist(EmailRepositoryError::Storage {
            source: anyhow::anyhow!("db down"),
        });
        assert!(err.is_transient());
    }

    #[test]
    fn is_transient_enqueue_is_true() {
        let err = SubmitEmailError::Enqueue(EmailQueueError::Storage {
            source: anyhow::anyhow!("queue down"),
        });
        assert!(err.is_transient());
    }

    #[test]
    fn is_transient_attachment_store_is_true() {
        let err = SubmitEmailError::AttachmentStore {
            source: anyhow::anyhow!("store I/O error"),
        };
        assert!(err.is_transient());
    }

    #[test]
    fn is_transient_attachment_fetch_is_false() {
        let err = SubmitEmailError::AttachmentFetch {
            source: anyhow::anyhow!("404 not found"),
        };
        assert!(!err.is_transient());
    }

    #[tokio::test]
    async fn duplicate_idempotency_key_does_not_call_attachment_store_put() {
        let existing_id = EmailId::default();
        let queue = FakeQueue::new();
        let store = FakeAttachmentStore::new();
        let put_count = store.put_count.clone();
        let mut input = make_input("sender@example.com");
        input.attachments.push(AttachmentInput::Inline {
            content_id: None,
            filename: "idem.txt".into(),
            content_type: "text/plain".into(),
            bytes: bytes::Bytes::from_static(b"x"),
        });
        let service = SubmitEmailService::new(
            DuplicatingRepository { existing_id },
            queue.clone(),
            FakeEventPublisher::new(),
            store,
            FakeFetcher,
        );
        service.execute(input).await.unwrap();
        assert_eq!(
            *put_count.lock().unwrap(),
            0,
            "attachment store put must not be called for a duplicate submission"
        );
    }
}
