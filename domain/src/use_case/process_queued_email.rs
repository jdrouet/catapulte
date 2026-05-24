use thiserror::Error;

use crate::entity::envelope::Envelope;
use crate::port::email_sender::{EmailSender, OutboundEmail, SendError};
use crate::port::template_interpolator::{InterpolateError, TemplateInterpolator};
use crate::port::template_renderer::{RenderError, TemplateRenderer};
use crate::port::template_resolver::{ResolveError, TemplateResolver};

pub trait ProcessQueuedEmailUseCase: Send + Sync + 'static {
    /// # Errors
    ///
    /// Returns a `ProcessQueuedEmailError` if resolve, interpolate, render, or send fails.
    fn execute(
        &self,
        envelope: Envelope,
    ) -> impl std::future::Future<Output = Result<(), ProcessQueuedEmailError>> + Send;
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
}

pub struct ProcessQueuedEmailService<R, I, P, S> {
    resolver: R,
    interpolator: I,
    renderer: P,
    sender: S,
}

impl<R, I, P, S> ProcessQueuedEmailService<R, I, P, S>
where
    R: TemplateResolver,
    I: TemplateInterpolator,
    P: TemplateRenderer,
    S: EmailSender,
{
    pub fn new(resolver: R, interpolator: I, renderer: P, sender: S) -> Self {
        Self {
            resolver,
            interpolator,
            renderer,
            sender,
        }
    }

    /// # Errors
    ///
    /// Returns a `ProcessQueuedEmailError` if the body fails to resolve, interpolate, render, or send.
    pub async fn execute(&self, envelope: Envelope) -> Result<(), ProcessQueuedEmailError> {
        // TODO: persist failures, emit lifecycle events, requeue retryable errors.
        let Envelope {
            sender,
            subject,
            recipients,
            body,
            variables,
            ..
        } = envelope;
        let resolved = self.resolver.resolve(body).await?;
        let interpolated = self.interpolator.interpolate(resolved, &variables)?;
        let rendered = self.renderer.render(interpolated)?;
        self.sender
            .send(OutboundEmail {
                sender,
                subject,
                recipients,
                body: rendered,
            })
            .await?;
        Ok(())
    }
}

impl<R, I, P, S> ProcessQueuedEmailUseCase for ProcessQueuedEmailService<R, I, P, S>
where
    R: TemplateResolver + Send + Sync + 'static,
    I: TemplateInterpolator + Send + Sync + 'static,
    P: TemplateRenderer + Send + Sync + 'static,
    S: EmailSender + Send + Sync + 'static,
{
    fn execute(
        &self,
        envelope: Envelope,
    ) -> impl std::future::Future<Output = Result<(), ProcessQueuedEmailError>> + Send {
        Self::execute(self, envelope)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Map, Value};

    use crate::entity::body::{
        BodySource, InterpolatedBody, MjmlSource, Plain, RenderedBody, ResolvedBody,
    };
    use crate::entity::email::RecipientKind;
    use crate::entity::envelope::Envelope;
    use crate::port::email_sender::{EmailSender, OutboundEmail, SendError};
    use crate::port::template_interpolator::{InterpolateError, TemplateInterpolator};
    use crate::port::template_renderer::{RenderError, TemplateRenderer};
    use crate::port::template_resolver::{ResolveError, TemplateResolver};

    use super::{ProcessQueuedEmailError, ProcessQueuedEmailService};

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
        fn render(&self, body: InterpolatedBody) -> Result<RenderedBody, RenderError> {
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
        async fn send(&self, _email: OutboundEmail) -> Result<(), SendError> {
            Ok(())
        }
    }

    struct FailingSender;

    impl EmailSender for FailingSender {
        async fn send(&self, _email: OutboundEmail) -> Result<(), SendError> {
            Err(SendError::Send {
                source: anyhow::anyhow!("connection refused"),
            })
        }
    }

    fn default_envelope(body: BodySource) -> Envelope {
        Envelope {
            idempotency_key: None,
            subject: None,
            sender: "sender@example.com".into(),
            recipients: vec![(RecipientKind::To, "to@example.com".into())],
            body,
            variables: Map::new(),
        }
    }

    fn default_envelope_with_vars(body: BodySource, variables: Map<String, Value>) -> Envelope {
        Envelope {
            idempotency_key: None,
            subject: None,
            sender: "sender@example.com".into(),
            recipients: vec![(RecipientKind::To, "to@example.com".into())],
            body,
            variables,
        }
    }

    fn default_service()
    -> ProcessQueuedEmailService<FakeResolver, FakeInterpolator, FakeRenderer, FakeSender> {
        ProcessQueuedEmailService::new(
            FakeResolver {
                inline_mjml: String::new(),
            },
            FakeInterpolator,
            FakeRenderer,
            FakeSender,
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
        );
        let body = BodySource::Plain(Plain::try_new(Some("hello".into()), None).unwrap());
        let envelope = default_envelope(body);
        let err = service.execute(envelope).await.unwrap_err();
        assert!(matches!(err, ProcessQueuedEmailError::Send(_)));
    }
}
