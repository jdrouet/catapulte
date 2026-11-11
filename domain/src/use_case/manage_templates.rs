use std::sync::Arc;

use thiserror::Error;

use crate::entity::body::{BodySource, InterpolatedBody, MjmlSource};
use crate::port::clock::Clock;
use crate::port::template_interpolator::TemplateInterpolator;
use crate::port::template_renderer::TemplateRenderer;
use crate::port::template_repository::{
    StoredTemplate, TemplateRepository, TemplateRepositoryError,
};
use crate::port::template_resolver::{ResolveError, TemplateResolver};
use crate::port::template_validator::TemplateValidator;
use crate::service::stored_template_resolver::StoredTemplateResolver;
use crate::use_case::submit_email::error_chain;

#[derive(Debug, Error)]
pub enum ManageTemplatesError {
    /// The name belongs to a template file on disk, which is authoritative.
    #[error("template {name:?} is a file on disk and cannot be changed here")]
    OnDisk { name: String },
    #[error("template {name:?} not found")]
    NotFound { name: String },
    /// The source does not parse, render or compile; the message says why.
    #[error("invalid template: {message}")]
    Invalid { message: String },
    #[error(transparent)]
    Repository(#[from] TemplateRepositoryError),
    #[error(transparent)]
    Resolve(#[from] ResolveError),
}

/// A rendered template, produced without sending anything.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TemplatePreview {
    pub html: String,
    pub text: Option<String>,
}

pub trait ManageTemplatesUseCase: Send + Sync + 'static {
    /// # Errors
    ///
    /// Returns `ManageTemplatesError::Repository` when the query fails.
    fn list(
        &self,
    ) -> impl std::future::Future<Output = Result<Vec<StoredTemplate>, ManageTemplatesError>> + Send;

    /// # Errors
    ///
    /// Returns `NotFound` when no stored template has that name.
    fn get(
        &self,
        name: String,
    ) -> impl std::future::Future<Output = Result<StoredTemplate, ManageTemplatesError>> + Send;

    /// Validates `source` and stores it as the next version of `name`.
    ///
    /// # Errors
    ///
    /// Returns `OnDisk` for a name that exists on disk and `Invalid` when the source fails
    /// validation.
    fn put(
        &self,
        name: String,
        source: String,
        validation_variables: serde_json::Map<String, serde_json::Value>,
    ) -> impl std::future::Future<Output = Result<StoredTemplate, ManageTemplatesError>> + Send;

    /// Removes every stored version of `name`.
    ///
    /// # Errors
    ///
    /// Returns `OnDisk` for a name that exists on disk and `NotFound` when nothing is stored.
    fn delete(
        &self,
        name: String,
    ) -> impl std::future::Future<Output = Result<(), ManageTemplatesError>> + Send;

    /// Renders the template as a send would, with `variables`.
    ///
    /// # Errors
    ///
    /// Returns `NotFound` for an unknown name and `Invalid` when rendering fails.
    fn preview(
        &self,
        name: String,
        variables: serde_json::Map<String, serde_json::Value>,
    ) -> impl std::future::Future<Output = Result<TemplatePreview, ManageTemplatesError>> + Send;
}

pub struct ManageTemplatesService<T, D, V, I, R, C> {
    repository: T,
    resolver: Arc<StoredTemplateResolver<T, D>>,
    validator: V,
    interpolator: I,
    renderer: R,
    clock: C,
}

impl<T, D, V, I, R, C> ManageTemplatesService<T, D, V, I, R, C> {
    /// `resolver` is the one sends use, so a preview renders what a send would.
    pub fn new(
        repository: T,
        resolver: Arc<StoredTemplateResolver<T, D>>,
        validator: V,
        interpolator: I,
        renderer: R,
        clock: C,
    ) -> Self {
        Self {
            repository,
            resolver,
            validator,
            interpolator,
            renderer,
            clock,
        }
    }
}

fn invalid(e: &dyn std::error::Error) -> ManageTemplatesError {
    ManageTemplatesError::Invalid {
        message: error_chain(e),
    }
}

impl<T, D, V, I, R, C> ManageTemplatesService<T, D, V, I, R, C>
where
    T: TemplateRepository,
    D: TemplateResolver,
    V: TemplateValidator,
    I: TemplateInterpolator,
    R: TemplateRenderer,
    C: Clock,
{
    async fn ensure_not_on_disk(&self, name: &str) -> Result<(), ManageTemplatesError> {
        if self.resolver.on_disk(name).await? {
            return Err(ManageTemplatesError::OnDisk {
                name: name.to_owned(),
            });
        }
        Ok(())
    }
}

impl<T, D, V, I, R, C> ManageTemplatesUseCase for ManageTemplatesService<T, D, V, I, R, C>
where
    T: TemplateRepository,
    D: TemplateResolver,
    V: TemplateValidator,
    I: TemplateInterpolator,
    R: TemplateRenderer,
    C: Clock,
{
    async fn list(&self) -> Result<Vec<StoredTemplate>, ManageTemplatesError> {
        Ok(self.repository.list().await?)
    }

    async fn get(&self, name: String) -> Result<StoredTemplate, ManageTemplatesError> {
        self.repository
            .latest(&name)
            .await?
            .ok_or(ManageTemplatesError::NotFound { name })
    }

    async fn put(
        &self,
        name: String,
        source: String,
        validation_variables: serde_json::Map<String, serde_json::Value>,
    ) -> Result<StoredTemplate, ManageTemplatesError> {
        self.ensure_not_on_disk(&name).await?;
        let interpolated = self
            .validator
            .validate(&source, &validation_variables)
            .map_err(|e| invalid(&e))?;
        self.renderer
            .render(InterpolatedBody::Mjml(interpolated))
            .await
            .map_err(|e| invalid(&e))?;
        Ok(self
            .repository
            .put(&name, &source, self.clock.now_ms())
            .await?)
    }

    async fn delete(&self, name: String) -> Result<(), ManageTemplatesError> {
        self.ensure_not_on_disk(&name).await?;
        if self.repository.delete(&name).await? {
            Ok(())
        } else {
            Err(ManageTemplatesError::NotFound { name })
        }
    }

    async fn preview(
        &self,
        name: String,
        variables: serde_json::Map<String, serde_json::Value>,
    ) -> Result<TemplatePreview, ManageTemplatesError> {
        let resolved = match self
            .resolver
            .resolve(BodySource::Mjml(MjmlSource::Named(name.clone())))
            .await
        {
            Ok(body) => body,
            Err(ResolveError::NotFound { .. }) => {
                return Err(ManageTemplatesError::NotFound { name });
            }
            Err(e) => return Err(e.into()),
        };
        let interpolated = self
            .interpolator
            .interpolate(resolved, &variables)
            .map_err(|e| invalid(&e))?;
        let rendered = self
            .renderer
            .render(interpolated)
            .await
            .map_err(|e| invalid(&e))?;
        Ok(TemplatePreview {
            html: rendered.html().unwrap_or_default().to_owned(),
            text: rendered.text().map(str::to_owned),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::entity::body::{Plain, RenderedBody, ResolvedBody};
    use crate::port::template_interpolator::InterpolateError;
    use crate::port::template_renderer::RenderError;
    use crate::service::stored_template_resolver::fakes::{FakeDisk, FakeRepo};

    struct Validator;

    impl TemplateValidator for Validator {
        fn validate(
            &self,
            source: &str,
            _: &serde_json::Map<String, serde_json::Value>,
        ) -> Result<String, InterpolateError> {
            if source.contains("{% for") {
                return Err(InterpolateError::BudgetExceeded);
            }
            if source.contains("{{ boom") {
                return Err(InterpolateError::Engine {
                    source: anyhow::anyhow!("syntax error"),
                });
            }
            Ok(source.to_owned())
        }
    }

    struct Interpolator;

    impl TemplateInterpolator for Interpolator {
        fn interpolate(
            &self,
            body: ResolvedBody,
            _: &serde_json::Map<String, serde_json::Value>,
        ) -> Result<InterpolatedBody, InterpolateError> {
            match body {
                ResolvedBody::Mjml(s) => Ok(InterpolatedBody::Mjml(s)),
                ResolvedBody::Plain(p) => Ok(InterpolatedBody::Plain(p)),
            }
        }
    }

    struct Renderer;

    impl TemplateRenderer for Renderer {
        async fn render(&self, body: InterpolatedBody) -> Result<RenderedBody, RenderError> {
            match body {
                InterpolatedBody::Mjml(s) if s.contains("<bad") => Err(RenderError::Mjml {
                    source: anyhow::anyhow!("bad mjml"),
                }),
                InterpolatedBody::Mjml(s) => Ok(RenderedBody::new(
                    Plain::try_new(Some("preview".to_owned()), Some(s)).unwrap(),
                )),
                InterpolatedBody::Plain(p) => Ok(RenderedBody::new(p)),
            }
        }
    }

    struct Clk;

    impl Clock for Clk {
        fn now_ms(&self) -> i64 {
            7
        }
    }

    type Service =
        ManageTemplatesService<FakeRepo, FakeDisk, Validator, Interpolator, Renderer, Clk>;

    fn service(repo: &FakeRepo, files: &[(&str, &str)]) -> Service {
        let disk = FakeDisk(
            files
                .iter()
                .map(|(n, s)| ((*n).to_owned(), (*s).to_owned()))
                .collect::<HashMap<_, _>>(),
        );
        ManageTemplatesService::new(
            repo.clone(),
            Arc::new(StoredTemplateResolver::new(repo.clone(), disk)),
            Validator,
            Interpolator,
            Renderer,
            Clk,
        )
    }

    fn vars() -> serde_json::Map<String, serde_json::Value> {
        serde_json::Map::new()
    }

    #[tokio::test]
    async fn put_stores_versions_and_get_returns_the_latest() {
        let repo = FakeRepo::default();
        let svc = service(&repo, &[]);

        let first = svc
            .put("welcome".into(), "<mjml>1</mjml>".into(), vars())
            .await
            .unwrap();
        let second = svc
            .put("welcome".into(), "<mjml>2</mjml>".into(), vars())
            .await
            .unwrap();

        assert_eq!(
            (first.version, second.version, second.created_at_ms),
            (1, 2, 7)
        );
        assert_eq!(
            svc.get("welcome".into()).await.unwrap().source,
            "<mjml>2</mjml>"
        );
        assert_eq!(svc.list().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_name_on_disk_cannot_be_put_or_deleted() {
        let repo = FakeRepo::with("welcome", "db");
        let svc = service(&repo, &[("welcome", "disk")]);

        let put = svc
            .put("welcome".into(), "<mjml/>".into(), vars())
            .await
            .unwrap_err();
        let delete = svc.delete("welcome".into()).await.unwrap_err();

        assert!(matches!(put, ManageTemplatesError::OnDisk { .. }));
        assert!(matches!(delete, ManageTemplatesError::OnDisk { .. }));
        assert_eq!(
            repo.0.lock().unwrap()["welcome"].source,
            "db",
            "nothing changed"
        );
    }

    #[tokio::test]
    async fn invalid_sources_are_rejected_with_the_reason_and_not_stored() {
        let repo = FakeRepo::default();
        let svc = service(&repo, &[]);

        let syntax = svc
            .put("a".into(), "{{ boom".into(), vars())
            .await
            .unwrap_err();
        let mjml = svc
            .put("b".into(), "<bad".into(), vars())
            .await
            .unwrap_err();

        assert!(
            matches!(&syntax, ManageTemplatesError::Invalid { message } if message.contains("syntax error"))
        );
        assert!(
            matches!(&mjml, ManageTemplatesError::Invalid { message } if message.contains("bad mjml"))
        );
        assert!(repo.0.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_template_that_exhausts_the_render_budget_is_rejected_with_that_message() {
        let repo = FakeRepo::default();
        let svc = service(&repo, &[]);

        let err = svc
            .put("loop".into(), "{% for".into(), vars())
            .await
            .unwrap_err();

        assert!(
            matches!(&err, ManageTemplatesError::Invalid { message } if message.contains("budget")),
            "{err}"
        );
        assert!(repo.0.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn delete_and_get_report_unknown_names() {
        let repo = FakeRepo::with("welcome", "<mjml/>");
        let svc = service(&repo, &[]);

        svc.delete("welcome".into()).await.unwrap();

        assert!(matches!(
            svc.delete("welcome".into()).await.unwrap_err(),
            ManageTemplatesError::NotFound { .. }
        ));
        assert!(matches!(
            svc.get("welcome".into()).await.unwrap_err(),
            ManageTemplatesError::NotFound { .. }
        ));
    }

    #[tokio::test]
    async fn preview_renders_stored_and_disk_templates_and_reports_unknown_ones() {
        let repo = FakeRepo::with("stored", "<mjml>db</mjml>");
        let svc = service(&repo, &[("filed", "<mjml>disk</mjml>")]);

        let stored = svc.preview("stored".into(), vars()).await.unwrap();
        let filed = svc.preview("filed".into(), vars()).await.unwrap();

        assert_eq!(stored.html, "<mjml>db</mjml>");
        assert_eq!(filed.html, "<mjml>disk</mjml>");
        assert_eq!(stored.text.as_deref(), Some("preview"));
        assert!(matches!(
            svc.preview("nope".into(), vars()).await.unwrap_err(),
            ManageTemplatesError::NotFound { .. }
        ));
    }
}
