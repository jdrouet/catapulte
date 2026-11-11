use thiserror::Error;
use tokio::io::AsyncReadExt;

use crate::entity::attachment::{AttachmentRef, ResolvedAttachment};
use crate::entity::email::EmailId;
use crate::entity::envelope::Envelope;
use crate::entity::error_class::ErrorClass;
use crate::entity::return_path::BounceConfig;
use crate::entity::sender::SenderName;
use crate::entity::suppression::suppression_key;
use crate::port::attachment_store::AttachmentStore;
use crate::port::email_sender::{EmailSender, OutboundEmail, SendError};
use crate::port::suppression_repository::{
    NoopSuppressionList, SuppressionRepository, SuppressionRepositoryError,
};
use crate::port::template_interpolator::{InterpolateError, TemplateInterpolator};
use crate::port::template_renderer::{RenderError, TemplateRenderer};
use crate::port::template_resolver::{ResolveError, TemplateResolver};

/// What a successful delivery reports back for the `delivery.succeeded` event.
#[derive(Debug)]
pub struct ProcessedEmail {
    pub sender_name: SenderName,
    pub smtp_response: Option<String>,
    pub message_id: String,
    /// Lowercased addresses dropped because they were suppressed since submission.
    pub suppressed: Vec<String>,
}

/// `<{email uuid}@{lowercased sender domain}>`. Derived only from the email id and sender,
/// so every retry or redelivery of the same email reuses the same Message-ID.
fn message_id(id: EmailId, sender: &str) -> String {
    let domain = crate::entity::email::bare_address(sender)
        .rsplit_once('@')
        .map_or_else(
            || "localhost".to_owned(),
            |(_, domain)| ascii_domain(domain),
        );
    format!("<{}@{domain}>", id.as_uuid())
}

/// Lowercase ASCII (IDNA) form of the domain, so the header needs no RFC 2047 encoding and
/// matches the event. A domain literal is kept as is; an unconvertible domain becomes `localhost`.
fn ascii_domain(domain: &str) -> String {
    if domain.starts_with('[') {
        return domain.to_ascii_lowercase();
    }
    idna::domain_to_ascii(domain)
        .ok()
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| "localhost".to_owned())
}

pub trait ProcessQueuedEmailUseCase: Send + Sync + 'static {
    fn execute(
        &self,
        id: EmailId,
        envelope: Envelope,
    ) -> impl std::future::Future<Output = Result<ProcessedEmail, ProcessQueuedEmailError>> + Send;
}

#[derive(Debug, Error)]
pub enum ProcessQueuedEmailError {
    #[error(transparent)]
    Resolve(#[from] ResolveError),
    #[error(transparent)]
    Interpolate(#[from] InterpolateError),
    #[error(transparent)]
    Render(#[from] RenderError),
    #[error(transparent)]
    Send(#[from] SendError),
    #[error("attachment resolve failed")]
    AttachmentResolve {
        #[source]
        source: anyhow::Error,
    },
    #[error("every recipient is on the suppression list")]
    AllRecipientsSuppressed,
    #[error(transparent)]
    Suppression(#[from] SuppressionRepositoryError),
}

impl ProcessQueuedEmailError {
    #[must_use]
    pub fn sender_name(&self) -> Option<&SenderName> {
        match self {
            Self::Send(e) => e.sender_name(),
            _ => None,
        }
    }

    /// True when retrying cannot succeed.
    #[must_use]
    pub fn is_permanent(&self) -> bool {
        match self {
            Self::Send(e) => !e.is_transient(),
            Self::AllRecipientsSuppressed => true,
            _ => false,
        }
    }

    #[must_use]
    pub fn error_class(&self) -> ErrorClass {
        match self {
            Self::Resolve(_) => ErrorClass::TemplateResolve,
            Self::Interpolate(_) => ErrorClass::TemplateInterpolate,
            Self::Render(_) => ErrorClass::TemplateRender,
            Self::AttachmentResolve { .. } => ErrorClass::Attachment,
            Self::Send(SendError::Send {
                permanent: true, ..
            }) => ErrorClass::Rejected,
            Self::Send(SendError::Send { .. }) | Self::Suppression(_) => ErrorClass::Delivery,
            Self::Send(SendError::NoMatchingRoute { .. }) => ErrorClass::Routing,
            Self::AllRecipientsSuppressed => ErrorClass::Suppressed,
        }
    }
}

pub struct ProcessQueuedEmailService<R, I, Rdr, S, A, X = NoopSuppressionList> {
    resolver: R,
    interpolator: I,
    renderer: Rdr,
    sender: S,
    attachment_store: A,
    suppressions: X,
    bounce: Option<BounceConfig>,
}

impl<R, I, Rdr, S, A> ProcessQueuedEmailService<R, I, Rdr, S, A, NoopSuppressionList> {
    pub fn new(
        resolver: R,
        interpolator: I,
        renderer: Rdr,
        sender: S,
        attachment_store: A,
    ) -> Self {
        Self {
            resolver,
            interpolator,
            renderer,
            sender,
            attachment_store,
            suppressions: NoopSuppressionList,
            bounce: None,
        }
    }
}

impl<R, I, Rdr, S, A, X> ProcessQueuedEmailService<R, I, Rdr, S, A, X> {
    /// Re-checks recipients against the suppression list before sending.
    #[must_use]
    pub fn with_suppression_list<Y>(
        self,
        suppressions: Y,
    ) -> ProcessQueuedEmailService<R, I, Rdr, S, A, Y> {
        ProcessQueuedEmailService {
            resolver: self.resolver,
            interpolator: self.interpolator,
            renderer: self.renderer,
            sender: self.sender,
            attachment_store: self.attachment_store,
            suppressions,
            bounce: self.bounce,
        }
    }

    /// Sends with a signed return path so bounces can be tied back to the email.
    #[must_use]
    pub fn with_bounce(mut self, bounce: BounceConfig) -> Self {
        self.bounce = Some(bounce);
        self
    }
}

impl<R, I, Rdr, S, A, X> ProcessQueuedEmailService<R, I, Rdr, S, A, X>
where
    R: TemplateResolver,
    I: TemplateInterpolator,
    Rdr: TemplateRenderer,
    S: EmailSender,
    A: AttachmentStore,
    X: SuppressionRepository,
{
    /// # Errors
    ///
    /// Returns a `ProcessQueuedEmailError` if the body fails to resolve, interpolate, render, or send.
    #[tracing::instrument(skip_all, name = "process_queued_email", fields(correlation_id = tracing::field::Empty))]
    pub async fn execute(
        &self,
        id: EmailId,
        envelope: Envelope,
    ) -> Result<ProcessedEmail, ProcessQueuedEmailError> {
        if let Some(ref cid) = envelope.correlation_id {
            tracing::Span::current().record("correlation_id", cid.as_str());
        }
        let Envelope {
            sender,
            subject,
            recipients,
            body,
            variables,
            attachments,
            reply_to,
            headers,
            unsubscribe,
            ..
        } = envelope;
        let keys: Vec<String> = recipients.iter().map(|(_, a)| suppression_key(a)).collect();
        let suppressed_set = self.suppressions.find_suppressed(&keys).await?;
        let remaining: Vec<_> = recipients
            .iter()
            .filter(|(_, a)| !suppressed_set.contains(&suppression_key(a)))
            .cloned()
            .collect();
        if remaining.is_empty() && !recipients.is_empty() {
            return Err(ProcessQueuedEmailError::AllRecipientsSuppressed);
        }
        let mut suppressed: Vec<String> = keys
            .into_iter()
            .filter(|k| suppressed_set.contains(k))
            .collect();
        suppressed.sort();
        suppressed.dedup();
        let recipients = remaining;
        let resolved = self.resolver.resolve(body).await?;
        let interpolated = self.interpolator.interpolate(resolved, &variables)?;
        let rendered = self.renderer.render(interpolated).await?;
        let resolved_attachments =
            resolve_attachments(&self.attachment_store, &attachments).await?;
        let message_id = message_id(id, &sender);
        let sent = self
            .sender
            .send(OutboundEmail {
                message_id: message_id.clone(),
                sender,
                subject,
                recipients,
                body: rendered,
                attachments: resolved_attachments,
                reply_to,
                headers,
                unsubscribe,
                envelope_from: self.bounce.as_ref().map(|b| b.return_path(id)),
            })
            .await?;
        Ok(ProcessedEmail {
            sender_name: sent.sender_name,
            smtp_response: sent.smtp_response,
            message_id,
            suppressed,
        })
    }
}

async fn resolve_attachments<A: AttachmentStore>(
    store: &A,
    refs: &[AttachmentRef],
) -> Result<Vec<ResolvedAttachment>, ProcessQueuedEmailError> {
    let mut resolved = Vec::with_capacity(refs.len());
    for att in refs {
        let mut reader =
            store
                .get(&att.blob)
                .await
                .map_err(|e| ProcessQueuedEmailError::AttachmentResolve {
                    source: anyhow::Error::new(e),
                })?;
        let mut buf = Vec::new();
        reader.read_to_end(&mut buf).await.map_err(|e| {
            ProcessQueuedEmailError::AttachmentResolve {
                source: anyhow::Error::new(e),
            }
        })?;
        resolved.push(ResolvedAttachment {
            filename: att.filename.clone(),
            content_type: att.content_type.clone(),
            bytes: bytes::Bytes::from(buf),
            content_id: att.content_id.clone(),
        });
    }
    Ok(resolved)
}

impl<R, I, Rdr, S, A, X> ProcessQueuedEmailUseCase for ProcessQueuedEmailService<R, I, Rdr, S, A, X>
where
    R: TemplateResolver,
    I: TemplateInterpolator,
    Rdr: TemplateRenderer,
    S: EmailSender,
    A: AttachmentStore,
    X: SuppressionRepository,
{
    fn execute(
        &self,
        id: EmailId,
        envelope: Envelope,
    ) -> impl std::future::Future<Output = Result<ProcessedEmail, ProcessQueuedEmailError>> + Send
    {
        Self::execute(self, id, envelope)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use serde_json::{Map, Value};

    use crate::entity::attachment::{AttachmentRef, BlobRef};
    use crate::entity::body::{
        BodySource, InterpolatedBody, MjmlSource, Plain, RenderedBody, ResolvedBody,
    };
    use crate::entity::email::EmailId;
    use crate::entity::email::RecipientKind;
    use crate::entity::envelope::Envelope;
    use crate::entity::return_path::BounceConfig;
    use crate::entity::sender::SenderName;
    use crate::port::attachment_store::{
        AttachmentReader, AttachmentStore, AttachmentStoreError, PutResult,
    };
    use crate::port::email_sender::{EmailSender, OutboundEmail, SendError, SentEmail};
    use crate::port::template_interpolator::{InterpolateError, TemplateInterpolator};
    use crate::port::template_renderer::{RenderError, TemplateRenderer};
    use crate::port::template_resolver::{ResolveError, TemplateResolver};

    use super::{ProcessQueuedEmailError, ProcessQueuedEmailService};

    type CapturingService = (
        ProcessQueuedEmailService<
            FakeResolver,
            FakeInterpolator,
            FakeRenderer,
            CapturingSender,
            FakeAttachmentStore,
        >,
        Arc<Mutex<Option<OutboundEmail>>>,
    );

    struct FakeAttachmentStore;

    #[allow(async_fn_in_trait)]
    impl AttachmentStore for FakeAttachmentStore {
        async fn put(&self, _reader: AttachmentReader) -> Result<PutResult, AttachmentStoreError> {
            Ok(PutResult {
                blob: BlobRef {
                    backend: "fake".into(),
                    key: "fake-key".into(),
                },
                size_bytes: 0,
            })
        }

        async fn get(&self, _blob: &BlobRef) -> Result<AttachmentReader, AttachmentStoreError> {
            Ok(Box::pin(std::io::Cursor::new(b"fake content".to_vec())))
        }

        async fn delete(&self, _blob: &BlobRef) -> Result<(), AttachmentStoreError> {
            Ok(())
        }
    }

    struct FakeResolver {
        inline_mjml: String,
    }

    impl TemplateResolver for FakeResolver {
        async fn resolve(&self, body: BodySource) -> Result<ResolvedBody, ResolveError> {
            match body {
                BodySource::Plain(p) => Ok(ResolvedBody::Plain(p)),
                BodySource::Mjml(MjmlSource::Inline(s)) => Ok(ResolvedBody::Mjml(s)),
                BodySource::Mjml(MjmlSource::Named(_) | MjmlSource::Remote(_)) => {
                    Ok(ResolvedBody::Mjml(self.inline_mjml.clone()))
                }
            }
        }
    }

    struct FailingResolver;

    impl TemplateResolver for FailingResolver {
        async fn resolve(&self, _body: BodySource) -> Result<ResolvedBody, ResolveError> {
            Err(ResolveError::NotFound {
                name: "missing".into(),
            })
        }
    }

    struct FakeInterpolator;

    fn apply_vars(s: &str, vars: &Map<String, Value>) -> String {
        let mut result = s.to_owned();
        for (k, v) in vars {
            let placeholder = format!("{{{{ {k} }}}}");
            let replacement = match v {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            result = result.replace(&placeholder, &replacement);
        }
        result
    }

    impl TemplateInterpolator for FakeInterpolator {
        fn interpolate(
            &self,
            body: ResolvedBody,
            variables: &Map<String, Value>,
        ) -> Result<InterpolatedBody, InterpolateError> {
            match body {
                ResolvedBody::Plain(p) => {
                    let (text, html) = p.into_parts();
                    let text = text.map(|t| apply_vars(&t, variables));
                    let html = html.map(|h| apply_vars(&h, variables));
                    let plain = Plain::try_new(text, html).expect("invariant preserved");
                    Ok(InterpolatedBody::Plain(plain))
                }
                ResolvedBody::Mjml(s) => Ok(InterpolatedBody::Mjml(apply_vars(&s, variables))),
            }
        }
    }

    struct FakeRenderer;

    impl TemplateRenderer for FakeRenderer {
        async fn render(&self, body: InterpolatedBody) -> Result<RenderedBody, RenderError> {
            match body {
                InterpolatedBody::Plain(p) => Ok(RenderedBody::new(p)),
                InterpolatedBody::Mjml(s) => {
                    let text = if let Some(start) = s.find("<mj-preview>") {
                        let after = &s[start + "<mj-preview>".len()..];
                        after
                            .find("</mj-preview>")
                            .map(|end| after[..end].to_owned())
                    } else {
                        None
                    };
                    let html = format!("<html>{s}</html>");
                    let plain = Plain::try_new(text, Some(html)).expect("html always present");
                    Ok(RenderedBody::new(plain))
                }
            }
        }
    }

    fn sent(name: &str) -> SentEmail {
        SentEmail {
            sender_name: SenderName::new(name),
            smtp_response: Some("250 2.0.0 Ok".to_owned()),
        }
    }

    struct FakeSender;

    impl EmailSender for FakeSender {
        async fn send(&self, _email: OutboundEmail) -> Result<SentEmail, SendError> {
            Ok(sent("fake"))
        }
    }

    struct CapturingSender {
        captured: Arc<Mutex<Option<OutboundEmail>>>,
    }

    impl CapturingSender {
        fn new() -> (Self, Arc<Mutex<Option<OutboundEmail>>>) {
            let captured = Arc::new(Mutex::new(None));
            (
                Self {
                    captured: Arc::clone(&captured),
                },
                captured,
            )
        }
    }

    #[allow(async_fn_in_trait)]
    impl EmailSender for CapturingSender {
        async fn send(&self, email: OutboundEmail) -> Result<SentEmail, SendError> {
            *self.captured.lock().unwrap() = Some(email);
            Ok(sent("capturing"))
        }
    }

    struct FailingSender;

    impl EmailSender for FailingSender {
        async fn send(&self, _email: OutboundEmail) -> Result<SentEmail, SendError> {
            Err(SendError::Send {
                sender_name: SenderName::new("failing"),
                permanent: false,
                source: anyhow::anyhow!("connection refused"),
            })
        }
    }

    struct FailingInterpolator;

    impl TemplateInterpolator for FailingInterpolator {
        fn interpolate(
            &self,
            _body: ResolvedBody,
            _variables: &Map<String, Value>,
        ) -> Result<InterpolatedBody, InterpolateError> {
            Err(InterpolateError::Engine {
                source: anyhow::anyhow!("interpolation failed"),
            })
        }
    }

    struct FailingRenderer;

    impl TemplateRenderer for FailingRenderer {
        async fn render(&self, _body: InterpolatedBody) -> Result<RenderedBody, RenderError> {
            Err(RenderError::Mjml {
                source: anyhow::anyhow!("render failed"),
            })
        }
    }

    fn default_envelope(body: BodySource) -> Envelope {
        Envelope::new(
            "sender@example.com",
            vec![(RecipientKind::To, "to@example.com".into())],
            body,
        )
    }

    fn default_envelope_with_vars(body: BodySource, variables: Map<String, Value>) -> Envelope {
        Envelope {
            variables,
            ..Envelope::new(
                "sender@example.com",
                vec![(RecipientKind::To, "to@example.com".into())],
                body,
            )
        }
    }

    fn default_service() -> ProcessQueuedEmailService<
        FakeResolver,
        FakeInterpolator,
        FakeRenderer,
        FakeSender,
        FakeAttachmentStore,
    > {
        ProcessQueuedEmailService::new(
            FakeResolver {
                inline_mjml: String::new(),
            },
            FakeInterpolator,
            FakeRenderer,
            FakeSender,
            FakeAttachmentStore,
        )
    }

    #[tokio::test]
    async fn plain_text_only_interpolated() {
        let service = default_service();
        let mut vars = Map::new();
        vars.insert("name".into(), Value::String("Jeremie".into()));
        let body = BodySource::Plain(Plain::try_new(Some("hi {{ name }}".into()), None).unwrap());
        let envelope = default_envelope_with_vars(body, vars);
        service.execute(EmailId::default(), envelope).await.unwrap();
    }

    #[tokio::test]
    async fn plain_both_text_and_html_interpolated() {
        let service = default_service();
        let mut vars = Map::new();
        vars.insert("x".into(), Value::String("world".into()));
        let body = BodySource::Plain(
            Plain::try_new(
                Some("hello {{ x }}".into()),
                Some("<p>hello {{ x }}</p>".into()),
            )
            .unwrap(),
        );
        let envelope = default_envelope_with_vars(body, vars);
        service.execute(EmailId::default(), envelope).await.unwrap();
    }

    #[tokio::test]
    async fn inline_mjml_with_preview_produces_text_and_html() {
        let mjml = "<mjml><mj-preview>Preview text</mj-preview></mjml>".to_owned();
        let service = default_service();
        let body = BodySource::Mjml(MjmlSource::Inline(mjml));
        let envelope = default_envelope(body);
        service.execute(EmailId::default(), envelope).await.unwrap();
    }

    #[tokio::test]
    async fn inline_mjml_without_preview_produces_html_only() {
        let mjml = "<mjml><mj-body></mj-body></mjml>".to_owned();
        let service = default_service();
        let body = BodySource::Mjml(MjmlSource::Inline(mjml));
        let envelope = default_envelope(body);
        service.execute(EmailId::default(), envelope).await.unwrap();
    }

    #[tokio::test]
    async fn named_mjml_resolved_then_rendered() {
        let service = ProcessQueuedEmailService::new(
            FakeResolver {
                inline_mjml: "<mjml><mj-preview>Hi</mj-preview></mjml>".into(),
            },
            FakeInterpolator,
            FakeRenderer,
            FakeSender,
            FakeAttachmentStore,
        );
        let body = BodySource::Mjml(MjmlSource::Named("welcome".into()));
        let envelope = default_envelope(body);
        service.execute(EmailId::default(), envelope).await.unwrap();
    }

    #[tokio::test]
    async fn resolver_failure_propagates_as_process_queued_email_error() {
        let service = ProcessQueuedEmailService::new(
            FailingResolver,
            FakeInterpolator,
            FakeRenderer,
            FakeSender,
            FakeAttachmentStore,
        );
        let body = BodySource::Plain(Plain::try_new(Some("hello".into()), None).unwrap());
        let envelope = default_envelope(body);
        let err = service
            .execute(EmailId::default(), envelope)
            .await
            .unwrap_err();
        assert!(matches!(err, ProcessQueuedEmailError::Resolve(_)));
    }

    #[tokio::test]
    async fn send_failure_propagates_as_process_queued_email_error() {
        let service = ProcessQueuedEmailService::new(
            FakeResolver {
                inline_mjml: String::new(),
            },
            FakeInterpolator,
            FakeRenderer,
            FailingSender,
            FakeAttachmentStore,
        );
        let body = BodySource::Plain(Plain::try_new(Some("hello".into()), None).unwrap());
        let envelope = default_envelope(body);
        let err = service
            .execute(EmailId::default(), envelope)
            .await
            .unwrap_err();
        assert!(matches!(err, ProcessQueuedEmailError::Send(_)));
    }

    #[tokio::test]
    async fn interpolate_failure_propagates_as_process_queued_email_error() {
        let service = ProcessQueuedEmailService::new(
            FakeResolver {
                inline_mjml: String::new(),
            },
            FailingInterpolator,
            FakeRenderer,
            FakeSender,
            FakeAttachmentStore,
        );
        let body = BodySource::Plain(Plain::try_new(Some("hello".into()), None).unwrap());
        let envelope = default_envelope(body);
        let err = service
            .execute(EmailId::default(), envelope)
            .await
            .unwrap_err();
        assert!(matches!(err, ProcessQueuedEmailError::Interpolate(_)));
    }

    #[tokio::test]
    async fn render_failure_propagates_as_process_queued_email_error() {
        let service = ProcessQueuedEmailService::new(
            FakeResolver {
                inline_mjml: String::new(),
            },
            FakeInterpolator,
            FailingRenderer,
            FakeSender,
            FakeAttachmentStore,
        );
        let body = BodySource::Plain(Plain::try_new(Some("hello".into()), None).unwrap());
        let envelope = default_envelope(body);
        let err = service
            .execute(EmailId::default(), envelope)
            .await
            .unwrap_err();
        assert!(matches!(err, ProcessQueuedEmailError::Render(_)));
    }

    fn capturing_service() -> CapturingService {
        let (sender, spy) = CapturingSender::new();
        let service = ProcessQueuedEmailService::new(
            FakeResolver {
                inline_mjml: String::new(),
            },
            FakeInterpolator,
            FakeRenderer,
            sender,
            FakeAttachmentStore,
        );
        (service, spy)
    }

    struct FixedSuppressions(Vec<&'static str>);

    #[allow(async_fn_in_trait)]
    impl crate::port::suppression_repository::SuppressionRepository for FixedSuppressions {
        async fn list(
            &self,
            _: crate::port::suppression_repository::ListSuppressionsParams,
        ) -> Result<
            Vec<crate::entity::suppression::Suppression>,
            crate::port::suppression_repository::SuppressionRepositoryError,
        > {
            Ok(vec![])
        }

        async fn add(
            &self,
            _: &[crate::entity::suppression::Suppression],
        ) -> Result<(), crate::port::suppression_repository::SuppressionRepositoryError> {
            Ok(())
        }

        async fn delete(
            &self,
            _: &str,
        ) -> Result<bool, crate::port::suppression_repository::SuppressionRepositoryError> {
            Ok(false)
        }

        async fn find_suppressed(
            &self,
            addresses: &[String],
        ) -> Result<
            std::collections::HashSet<String>,
            crate::port::suppression_repository::SuppressionRepositoryError,
        > {
            Ok(addresses
                .iter()
                .filter(|a| self.0.contains(&a.as_str()))
                .cloned()
                .collect())
        }
    }

    fn envelope_to(recipients: &[&str]) -> Envelope {
        let mut envelope = default_envelope(BodySource::Plain(
            Plain::try_new(Some("hello".into()), None).unwrap(),
        ));
        envelope.recipients = recipients
            .iter()
            .map(|a| (RecipientKind::To, (*a).to_owned()))
            .collect();
        envelope
    }

    #[tokio::test]
    async fn suppressed_recipients_are_dropped_before_sending() {
        let (service, spy) = capturing_service();
        let service = service.with_suppression_list(FixedSuppressions(vec!["b@x.com"]));

        let processed = service
            .execute(EmailId::default(), envelope_to(&["a@x.com", "B <B@x.com>"]))
            .await
            .unwrap();

        let sent = spy.lock().unwrap().take().unwrap();
        assert_eq!(sent.recipients.len(), 1);
        assert_eq!(sent.recipients[0].1, "a@x.com");
        assert_eq!(processed.suppressed, vec!["b@x.com".to_owned()]);
    }

    #[tokio::test]
    async fn idn_equivalent_recipients_are_all_suppressed_by_the_worker() {
        let (service, spy) = capturing_service();
        let service =
            service.with_suppression_list(FixedSuppressions(vec!["u@xn--bcher-kva.example"]));

        let err = service
            .execute(
                EmailId::default(),
                envelope_to(&[
                    "u@bücher.example",
                    "U@BÜCHER.example",
                    "u@xn--bcher-kva.example",
                ]),
            )
            .await
            .unwrap_err();

        assert!(matches!(
            err,
            ProcessQueuedEmailError::AllRecipientsSuppressed
        ));
        assert!(spy.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn bounce_config_sets_the_signed_return_path_and_suppressed_recipients_stay_out() {
        let bounce =
            BounceConfig::new("bounces.example.com", "0123456789abcdef0123456789abcdef").unwrap();
        let (service, spy) = capturing_service();
        let service = service
            .with_suppression_list(FixedSuppressions(vec!["b@x.com"]))
            .with_bounce(bounce.clone());
        let id = EmailId::default();

        service
            .execute(id, envelope_to(&["a@x.com", "b@x.com"]))
            .await
            .unwrap();

        let sent = spy.lock().unwrap().take().unwrap();
        assert_eq!(sent.envelope_from, Some(bounce.return_path(id)));
        let rcpts: Vec<_> = sent.recipients.iter().map(|(_, a)| a.as_str()).collect();
        assert_eq!(rcpts, ["a@x.com"]);
    }

    #[tokio::test]
    async fn without_bounce_config_there_is_no_return_path() {
        let (service, spy) = capturing_service();

        service
            .execute(EmailId::default(), envelope_to(&["a@x.com"]))
            .await
            .unwrap();

        assert_eq!(spy.lock().unwrap().take().unwrap().envelope_from, None);
    }

    #[tokio::test]
    async fn all_recipients_suppressed_is_permanent_and_sends_nothing() {
        let (service, spy) = capturing_service();
        let service = service.with_suppression_list(FixedSuppressions(vec!["a@x.com"]));

        let err = service
            .execute(EmailId::default(), envelope_to(&["a@x.com"]))
            .await
            .unwrap_err();

        assert!(matches!(
            err,
            ProcessQueuedEmailError::AllRecipientsSuppressed
        ));
        assert!(err.is_permanent());
        assert_eq!(
            err.error_class(),
            crate::entity::error_class::ErrorClass::Suppressed
        );
        assert!(spy.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn result_carries_sender_name_smtp_response_and_message_id() {
        let (service, spy) = capturing_service();
        let id = EmailId::default();
        let body = BodySource::Plain(Plain::try_new(Some("hello".into()), None).unwrap());
        let mut envelope = default_envelope(body);
        envelope.sender = "Sender <Sender@EXAMPLE.com>".into();

        let processed = service.execute(id, envelope).await.unwrap();

        let expected = format!("<{}@example.com>", id.as_uuid());
        assert_eq!(processed.message_id, expected);
        assert_eq!(processed.sender_name.as_str(), "capturing");
        assert_eq!(processed.smtp_response.as_deref(), Some("250 2.0.0 Ok"));
        assert_eq!(spy.lock().unwrap().as_ref().unwrap().message_id, expected);
    }

    #[tokio::test]
    async fn message_id_is_stable_across_retries_and_differs_per_email() {
        let (service, _spy) = capturing_service();
        let id = EmailId::default();
        let body = || BodySource::Plain(Plain::try_new(Some("hello".into()), None).unwrap());

        let first = service.execute(id, default_envelope(body())).await.unwrap();
        let retry = service.execute(id, default_envelope(body())).await.unwrap();
        let other = service
            .execute(EmailId::default(), default_envelope(body()))
            .await
            .unwrap();

        assert_eq!(first.message_id, retry.message_id);
        assert_ne!(first.message_id, other.message_id);
    }

    #[tokio::test]
    async fn idn_sender_gets_a_punycode_message_id_on_the_email_and_the_result() {
        let (service, spy) = capturing_service();
        let id = EmailId::default();
        let body = BodySource::Plain(Plain::try_new(Some("hello".into()), None).unwrap());
        let mut envelope = default_envelope(body);
        envelope.sender = "Büro <a@Bücher.DE>".into();

        let processed = service.execute(id, envelope).await.unwrap();

        let expected = format!("<{}@xn--bcher-kva.de>", id.as_uuid());
        assert_eq!(processed.message_id, expected);
        assert_eq!(spy.lock().unwrap().as_ref().unwrap().message_id, expected);
    }

    #[test]
    fn message_id_handles_ip_literal_and_missing_domains() {
        let id = EmailId::default();
        assert!(super::message_id(id, "a@[192.0.2.1]").ends_with("@[192.0.2.1]>"));
        assert!(super::message_id(id, "no-at-sign").ends_with("@localhost>"));
    }

    #[tokio::test]
    async fn plain_text_only_outbound_email_has_correct_sender() {
        let (service, spy) = capturing_service();
        let body = BodySource::Plain(Plain::try_new(Some("hello".into()), None).unwrap());
        let envelope = default_envelope(body);
        service.execute(EmailId::default(), envelope).await.unwrap();
        let captured = spy.lock().unwrap();
        let email = captured.as_ref().unwrap();
        assert_eq!(email.sender, "sender@example.com");
    }

    #[tokio::test]
    async fn plain_text_only_outbound_email_has_correct_body_text() {
        let (service, spy) = capturing_service();
        let mut vars = Map::new();
        vars.insert("name".into(), Value::String("World".into()));
        let body = BodySource::Plain(Plain::try_new(Some("hi {{ name }}".into()), None).unwrap());
        let envelope = default_envelope_with_vars(body, vars);
        service.execute(EmailId::default(), envelope).await.unwrap();
        let captured = spy.lock().unwrap();
        let email = captured.as_ref().unwrap();
        let plain = email.body.text();
        assert_eq!(plain, Some("hi World"));
        assert_eq!(email.body.html(), None);
    }

    #[tokio::test]
    async fn interpolation_applied_to_html_part() {
        let (service, spy) = capturing_service();
        let mut vars = Map::new();
        vars.insert("greeting".into(), Value::String("hello".into()));
        let body = BodySource::Plain(
            Plain::try_new(Some("text".into()), Some("<p>{{ greeting }}</p>".into())).unwrap(),
        );
        let envelope = default_envelope_with_vars(body, vars);
        service.execute(EmailId::default(), envelope).await.unwrap();
        let captured = spy.lock().unwrap();
        let email = captured.as_ref().unwrap();
        assert_eq!(email.body.html(), Some("<p>hello</p>"));
    }

    #[tokio::test]
    async fn attachments_are_resolved_from_store() {
        let (service, spy) = capturing_service();
        let body = BodySource::Plain(Plain::try_new(Some("hello".into()), None).unwrap());
        let mut envelope = default_envelope(body);
        envelope.attachments.push(AttachmentRef {
            content_id: None,
            filename: "doc.txt".into(),
            content_type: "text/plain".into(),
            size_bytes: 12,
            blob: BlobRef {
                backend: "fake".into(),
                key: "fake-key".into(),
            },
        });
        service.execute(EmailId::default(), envelope).await.unwrap();
        let captured = spy.lock().unwrap();
        let email = captured.as_ref().unwrap();
        assert_eq!(email.attachments.len(), 1);
        assert_eq!(email.attachments[0].filename, "doc.txt");
        assert_eq!(email.attachments[0].bytes.as_ref(), b"fake content");
    }

    #[tokio::test]
    async fn resolve_failure_maps_to_template_resolve_error_class() {
        use crate::entity::error_class::ErrorClass;

        let service = ProcessQueuedEmailService::new(
            FailingResolver,
            FakeInterpolator,
            FakeRenderer,
            FakeSender,
            FakeAttachmentStore,
        );
        let body = BodySource::Plain(Plain::try_new(Some("hello".into()), None).unwrap());
        let envelope = default_envelope(body);
        let err = service
            .execute(EmailId::default(), envelope)
            .await
            .unwrap_err();
        assert_eq!(err.error_class(), ErrorClass::TemplateResolve);
    }

    #[tokio::test]
    async fn interpolate_failure_maps_to_template_interpolate_error_class() {
        use crate::entity::error_class::ErrorClass;

        let service = ProcessQueuedEmailService::new(
            FakeResolver {
                inline_mjml: String::new(),
            },
            FailingInterpolator,
            FakeRenderer,
            FakeSender,
            FakeAttachmentStore,
        );
        let body = BodySource::Plain(Plain::try_new(Some("hello".into()), None).unwrap());
        let envelope = default_envelope(body);
        let err = service
            .execute(EmailId::default(), envelope)
            .await
            .unwrap_err();
        assert_eq!(err.error_class(), ErrorClass::TemplateInterpolate);
    }

    #[tokio::test]
    async fn render_failure_maps_to_template_render_error_class() {
        use crate::entity::error_class::ErrorClass;

        let service = ProcessQueuedEmailService::new(
            FakeResolver {
                inline_mjml: String::new(),
            },
            FakeInterpolator,
            FailingRenderer,
            FakeSender,
            FakeAttachmentStore,
        );
        let body = BodySource::Plain(Plain::try_new(Some("hello".into()), None).unwrap());
        let envelope = default_envelope(body);
        let err = service
            .execute(EmailId::default(), envelope)
            .await
            .unwrap_err();
        assert_eq!(err.error_class(), ErrorClass::TemplateRender);
    }

    #[tokio::test]
    async fn send_failure_maps_to_delivery_error_class() {
        use crate::entity::error_class::ErrorClass;

        let service = ProcessQueuedEmailService::new(
            FakeResolver {
                inline_mjml: String::new(),
            },
            FakeInterpolator,
            FakeRenderer,
            FailingSender,
            FakeAttachmentStore,
        );
        let body = BodySource::Plain(Plain::try_new(Some("hello".into()), None).unwrap());
        let envelope = default_envelope(body);
        let err = service
            .execute(EmailId::default(), envelope)
            .await
            .unwrap_err();
        assert_eq!(err.error_class(), ErrorClass::Delivery);
    }

    struct PermanentSender;

    impl EmailSender for PermanentSender {
        async fn send(&self, _email: OutboundEmail) -> Result<SentEmail, SendError> {
            Err(SendError::Send {
                sender_name: SenderName::new("failing"),
                permanent: true,
                source: anyhow::anyhow!("550 5.1.1 unknown user"),
            })
        }
    }

    #[tokio::test]
    async fn permanent_send_failure_maps_to_rejected_error_class() {
        use crate::entity::error_class::ErrorClass;

        let service = ProcessQueuedEmailService::new(
            FakeResolver {
                inline_mjml: String::new(),
            },
            FakeInterpolator,
            FakeRenderer,
            PermanentSender,
            FakeAttachmentStore,
        );
        let body = BodySource::Plain(Plain::try_new(Some("hello".into()), None).unwrap());
        let err = service
            .execute(EmailId::default(), default_envelope(body))
            .await
            .unwrap_err();
        assert_eq!(err.error_class(), ErrorClass::Rejected);
        assert!(err.to_string().contains("550"), "{err}");
    }

    /// Refuses, as a message-level failure, rendered bodies larger than `max`.
    struct SizeLimitedTransport {
        max: usize,
    }

    impl crate::port::email_transport::EmailTransport for SizeLimitedTransport {
        async fn deliver<'a>(
            &'a self,
            email: &'a OutboundEmail,
        ) -> Result<
            crate::port::email_transport::DeliveryReceipt,
            crate::port::email_transport::TransportError,
        > {
            let size =
                email.body.html().map_or(0, str::len) + email.body.text().map_or(0, str::len);
            if size > self.max {
                return Err(crate::port::email_transport::TransportError {
                    kind: crate::port::email_transport::TransportErrorKind::PermanentMessage,
                    source: anyhow::anyhow!("rendered message exceeds CATAPULTE_MAX_MESSAGE_BYTES"),
                });
            }
            Ok(crate::port::email_transport::DeliveryReceipt::default())
        }
    }

    /// Chain below the worker: a small template with a huge variable renders past the limit,
    /// the size-enforcing transport refuses it, and the result is a terminal `rejected` error
    /// (the worker acks non-transient errors and emits `delivery.failed`, see its tests).
    #[tokio::test]
    async fn huge_variable_in_small_template_ends_as_terminal_rejected() {
        use crate::entity::error_class::ErrorClass;
        use crate::port::clock::SystemClock;
        use crate::service::routed_email_sender::{
            NoopSenderUsage, RoutedEmailSender, SenderRoute,
        };

        let sender = RoutedEmailSender::new(
            vec![SenderRoute {
                name: SenderName::new("primary"),
                priority: 1,
                quota: None,
                match_sender_domain: None,
                transport: SizeLimitedTransport { max: 1000 },
            }],
            NoopSenderUsage,
            SystemClock,
        )
        .unwrap();
        let service = ProcessQueuedEmailService::new(
            FakeResolver {
                inline_mjml: String::new(),
            },
            FakeInterpolator,
            FakeRenderer,
            sender,
            FakeAttachmentStore,
        );
        let body = BodySource::Mjml(MjmlSource::Inline("<mj-text>{{ big }}</mj-text>".into()));
        let mut vars = Map::new();
        vars.insert("big".into(), Value::String("x".repeat(10_000)));

        let err = service
            .execute(EmailId::default(), default_envelope_with_vars(body, vars))
            .await
            .unwrap_err();

        assert_eq!(err.error_class(), ErrorClass::Rejected);
        assert!(matches!(&err, ProcessQueuedEmailError::Send(s) if !s.is_transient()));
    }

    struct NoMatchingRouteSender;

    impl EmailSender for NoMatchingRouteSender {
        async fn send(&self, _email: OutboundEmail) -> Result<SentEmail, SendError> {
            Err(SendError::NoMatchingRoute {
                sender_domain: "example.com".to_owned(),
            })
        }
    }

    #[tokio::test]
    async fn no_matching_route_maps_to_routing_error_class() {
        use crate::entity::error_class::ErrorClass;

        let service = ProcessQueuedEmailService::new(
            FakeResolver {
                inline_mjml: String::new(),
            },
            FakeInterpolator,
            FakeRenderer,
            NoMatchingRouteSender,
            FakeAttachmentStore,
        );
        let body = BodySource::Plain(Plain::try_new(Some("hello".into()), None).unwrap());
        let envelope = default_envelope(body);
        let err = service
            .execute(EmailId::default(), envelope)
            .await
            .unwrap_err();
        assert_eq!(err.error_class(), ErrorClass::Routing);
    }
}
