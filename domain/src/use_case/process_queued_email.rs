use thiserror::Error;
use tokio::io::AsyncReadExt;

use crate::entity::attachment::{AttachmentRef, ResolvedAttachment};
use crate::entity::envelope::Envelope;
use crate::entity::sender::SenderName;
use crate::port::attachment_store::AttachmentStore;
use crate::port::email_sender::{EmailSender, OutboundEmail, SendError};
use crate::port::template_interpolator::{InterpolateError, TemplateInterpolator};
use crate::port::template_renderer::{RenderError, TemplateRenderer};
use crate::port::template_resolver::{ResolveError, TemplateResolver};

pub trait ProcessQueuedEmailUseCase: Send + Sync + 'static {
    fn execute(
        &self,
        envelope: Envelope,
    ) -> impl std::future::Future<Output = Result<SenderName, ProcessQueuedEmailError>> + Send;
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
}

impl ProcessQueuedEmailError {
    #[must_use]
    pub fn sender_name(&self) -> Option<&SenderName> {
        match self {
            Self::Send(e) => e.sender_name(),
            _ => None,
        }
    }
}

pub struct ProcessQueuedEmailService<R, I, Rdr, S, A> {
    resolver: R,
    interpolator: I,
    renderer: Rdr,
    sender: S,
    attachment_store: A,
}

impl<R, I, Rdr, S, A> ProcessQueuedEmailService<R, I, Rdr, S, A>
where
    R: TemplateResolver,
    I: TemplateInterpolator,
    Rdr: TemplateRenderer,
    S: EmailSender,
    A: AttachmentStore,
{
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
        }
    }

    /// # Errors
    ///
    /// Returns a `ProcessQueuedEmailError` if the body fails to resolve, interpolate, render, or send.
    pub async fn execute(&self, envelope: Envelope) -> Result<SenderName, ProcessQueuedEmailError> {
        let Envelope {
            sender,
            subject,
            recipients,
            body,
            variables,
            attachments,
            ..
        } = envelope;
        let resolved = self.resolver.resolve(body).await?;
        let interpolated = self.interpolator.interpolate(resolved, &variables)?;
        let rendered = self.renderer.render(interpolated).await?;
        let resolved_attachments =
            resolve_attachments(&self.attachment_store, &attachments).await?;
        let sender_name = self
            .sender
            .send(OutboundEmail {
                sender,
                subject,
                recipients,
                body: rendered,
                attachments: resolved_attachments,
            })
            .await?;
        Ok(sender_name)
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
        });
    }
    Ok(resolved)
}

impl<R, I, Rdr, S, A> ProcessQueuedEmailUseCase for ProcessQueuedEmailService<R, I, Rdr, S, A>
where
    R: TemplateResolver,
    I: TemplateInterpolator,
    Rdr: TemplateRenderer,
    S: EmailSender,
    A: AttachmentStore,
{
    fn execute(
        &self,
        envelope: Envelope,
    ) -> impl std::future::Future<Output = Result<SenderName, ProcessQueuedEmailError>> + Send {
        Self::execute(self, envelope)
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
    use crate::entity::email::RecipientKind;
    use crate::entity::envelope::Envelope;
    use crate::entity::sender::SenderName;
    use crate::port::attachment_store::{
        AttachmentReader, AttachmentStore, AttachmentStoreError, PutResult,
    };
    use crate::port::email_sender::{EmailSender, OutboundEmail, SendError};
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

    struct FakeSender;

    impl EmailSender for FakeSender {
        async fn send(&self, _email: OutboundEmail) -> Result<SenderName, SendError> {
            Ok(SenderName::new("fake"))
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
        async fn send(&self, email: OutboundEmail) -> Result<SenderName, SendError> {
            *self.captured.lock().unwrap() = Some(email);
            Ok(SenderName::new("capturing"))
        }
    }

    struct FailingSender;

    impl EmailSender for FailingSender {
        async fn send(&self, _email: OutboundEmail) -> Result<SenderName, SendError> {
            Err(SendError::Send {
                sender_name: SenderName::new("failing"),
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
        Envelope {
            idempotency_key: None,
            correlation_id: None,
            subject: None,
            sender: "sender@example.com".into(),
            recipients: vec![(RecipientKind::To, "to@example.com".into())],
            body,
            variables: Map::new(),
            attachments: vec![],
        }
    }

    fn default_envelope_with_vars(body: BodySource, variables: Map<String, Value>) -> Envelope {
        Envelope {
            idempotency_key: None,
            correlation_id: None,
            subject: None,
            sender: "sender@example.com".into(),
            recipients: vec![(RecipientKind::To, "to@example.com".into())],
            body,
            variables,
            attachments: vec![],
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
        service.execute(envelope).await.unwrap();
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
        service.execute(envelope).await.unwrap();
    }

    #[tokio::test]
    async fn inline_mjml_with_preview_produces_text_and_html() {
        let mjml = "<mjml><mj-preview>Preview text</mj-preview></mjml>".to_owned();
        let service = default_service();
        let body = BodySource::Mjml(MjmlSource::Inline(mjml));
        let envelope = default_envelope(body);
        service.execute(envelope).await.unwrap();
    }

    #[tokio::test]
    async fn inline_mjml_without_preview_produces_html_only() {
        let mjml = "<mjml><mj-body></mj-body></mjml>".to_owned();
        let service = default_service();
        let body = BodySource::Mjml(MjmlSource::Inline(mjml));
        let envelope = default_envelope(body);
        service.execute(envelope).await.unwrap();
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
        service.execute(envelope).await.unwrap();
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
        let err = service.execute(envelope).await.unwrap_err();
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
        let err = service.execute(envelope).await.unwrap_err();
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
        let err = service.execute(envelope).await.unwrap_err();
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
        let err = service.execute(envelope).await.unwrap_err();
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

    #[tokio::test]
    async fn plain_text_only_outbound_email_has_correct_sender() {
        let (service, spy) = capturing_service();
        let body = BodySource::Plain(Plain::try_new(Some("hello".into()), None).unwrap());
        let envelope = default_envelope(body);
        service.execute(envelope).await.unwrap();
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
        service.execute(envelope).await.unwrap();
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
        service.execute(envelope).await.unwrap();
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
            filename: "doc.txt".into(),
            content_type: "text/plain".into(),
            size_bytes: 12,
            blob: BlobRef {
                backend: "fake".into(),
                key: "fake-key".into(),
            },
        });
        service.execute(envelope).await.unwrap();
        let captured = spy.lock().unwrap();
        let email = captured.as_ref().unwrap();
        assert_eq!(email.attachments.len(), 1);
        assert_eq!(email.attachments[0].filename, "doc.txt");
        assert_eq!(email.attachments[0].bytes.as_ref(), b"fake content");
    }
}
