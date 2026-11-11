use crate::entity::body::{BodySource, MjmlSource, ResolvedBody};
use crate::port::template_repository::TemplateRepository;
use crate::port::template_resolver::{ResolveError, TemplateResolver};

/// Resolves named MJML templates from the files on disk first, and from the database only for
/// names the files do not have. Every other body goes straight to the inner resolver.
pub struct StoredTemplateResolver<Repo, Inner> {
    repository: Repo,
    inner: Inner,
}

impl<Repo, Inner> StoredTemplateResolver<Repo, Inner>
where
    Repo: TemplateRepository,
    Inner: TemplateResolver,
{
    pub fn new(repository: Repo, inner: Inner) -> Self {
        Self { repository, inner }
    }

    /// True when the inner resolver (the filesystem) already owns `name`.
    ///
    /// # Errors
    ///
    /// Returns the inner resolver's error when it fails for any reason but a missing template.
    pub async fn on_disk(&self, name: &str) -> Result<bool, ResolveError> {
        match self
            .inner
            .resolve(BodySource::Mjml(MjmlSource::Named(name.to_owned())))
            .await
        {
            Ok(_) => Ok(true),
            Err(ResolveError::NotFound { .. }) => Ok(false),
            Err(e) => Err(e),
        }
    }
}

impl<Repo, Inner> TemplateResolver for StoredTemplateResolver<Repo, Inner>
where
    Repo: TemplateRepository,
    Inner: TemplateResolver,
{
    async fn resolve(&self, body: BodySource) -> Result<ResolvedBody, ResolveError> {
        let BodySource::Mjml(MjmlSource::Named(name)) = &body else {
            return self.inner.resolve(body).await;
        };
        let name = name.clone();
        match self.inner.resolve(body).await {
            Err(ResolveError::NotFound { .. }) => {}
            other => return other,
        }
        match self.repository.latest(&name).await {
            Ok(Some(template)) => Ok(ResolvedBody::Mjml(template.source)),
            Ok(None) => Err(ResolveError::NotFound { name }),
            Err(e) => Err(ResolveError::Fetch {
                url: format!("stored template {name:?}"),
                source: anyhow::Error::new(e),
            }),
        }
    }
}

#[cfg(test)]
pub(crate) mod fakes {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use crate::entity::body::{BodySource, MjmlSource, ResolvedBody};
    use crate::port::template_repository::{
        StoredTemplate, TemplateRepository, TemplateRepositoryError,
    };
    use crate::port::template_resolver::{ResolveError, TemplateResolver};

    #[derive(Clone, Default)]
    pub(crate) struct FakeRepo(pub Arc<Mutex<HashMap<String, StoredTemplate>>>);

    impl FakeRepo {
        pub(crate) fn with(name: &str, source: &str) -> Self {
            let repo = Self::default();
            repo.0.lock().unwrap().insert(
                name.to_owned(),
                StoredTemplate {
                    name: name.to_owned(),
                    version: 1,
                    source: source.to_owned(),
                    created_at_ms: 0,
                },
            );
            repo
        }
    }

    impl TemplateRepository for FakeRepo {
        async fn latest(
            &self,
            name: &str,
        ) -> Result<Option<StoredTemplate>, TemplateRepositoryError> {
            Ok(self.0.lock().unwrap().get(name).cloned())
        }

        async fn list(&self) -> Result<Vec<StoredTemplate>, TemplateRepositoryError> {
            let mut all: Vec<_> = self.0.lock().unwrap().values().cloned().collect();
            all.sort_by(|a, b| a.name.cmp(&b.name));
            Ok(all)
        }

        async fn put(
            &self,
            name: &str,
            source: &str,
            created_at_ms: i64,
        ) -> Result<StoredTemplate, TemplateRepositoryError> {
            let mut map = self.0.lock().unwrap();
            let version = map.get(name).map_or(1, |t| t.version + 1);
            let template = StoredTemplate {
                name: name.to_owned(),
                version,
                source: source.to_owned(),
                created_at_ms,
            };
            map.insert(name.to_owned(), template.clone());
            Ok(template)
        }

        async fn delete(&self, name: &str) -> Result<bool, TemplateRepositoryError> {
            Ok(self.0.lock().unwrap().remove(name).is_some())
        }
    }

    /// Stands in for the filesystem resolver: knows the names in `files`.
    pub(crate) struct FakeDisk(pub HashMap<String, String>);

    impl TemplateResolver for FakeDisk {
        async fn resolve(&self, body: BodySource) -> Result<ResolvedBody, ResolveError> {
            match body {
                BodySource::Mjml(MjmlSource::Named(name)) => self
                    .0
                    .get(&name)
                    .map(|s| ResolvedBody::Mjml(s.clone()))
                    .ok_or(ResolveError::NotFound { name }),
                BodySource::Plain(p) => Ok(ResolvedBody::Plain(p)),
                BodySource::Mjml(MjmlSource::Inline(s)) => Ok(ResolvedBody::Mjml(s)),
                BodySource::Mjml(MjmlSource::Remote(_)) => unimplemented!(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fakes::{FakeDisk, FakeRepo};
    use super::*;
    use crate::entity::body::Plain;

    fn named(name: &str) -> BodySource {
        BodySource::Mjml(MjmlSource::Named(name.to_owned()))
    }

    fn source_of(body: ResolvedBody) -> String {
        match body {
            ResolvedBody::Mjml(s) => s,
            ResolvedBody::Plain(_) => panic!("expected mjml"),
        }
    }

    fn disk(files: &[(&str, &str)]) -> FakeDisk {
        FakeDisk(
            files
                .iter()
                .map(|(n, s)| ((*n).to_owned(), (*s).to_owned()))
                .collect(),
        )
    }

    #[tokio::test]
    async fn the_filesystem_wins_over_the_database() {
        let resolver = StoredTemplateResolver::new(
            FakeRepo::with("welcome", "from db"),
            disk(&[("welcome", "from disk")]),
        );

        let body = resolver.resolve(named("welcome")).await.unwrap();

        assert_eq!(source_of(body), "from disk");
        assert!(resolver.on_disk("welcome").await.unwrap());
    }

    #[tokio::test]
    async fn names_missing_on_disk_resolve_from_the_database() {
        let resolver = StoredTemplateResolver::new(FakeRepo::with("welcome", "from db"), disk(&[]));

        assert_eq!(
            source_of(resolver.resolve(named("welcome")).await.unwrap()),
            "from db"
        );
        assert!(!resolver.on_disk("welcome").await.unwrap());
        let err = resolver.resolve(named("nope")).await.unwrap_err();
        assert!(matches!(err, ResolveError::NotFound { name } if name == "nope"));
    }

    #[tokio::test]
    async fn other_bodies_go_to_the_inner_resolver() {
        let resolver = StoredTemplateResolver::new(FakeRepo::default(), disk(&[]));
        let plain = BodySource::Plain(Plain::try_new(Some("hi".into()), None).unwrap());

        assert!(matches!(
            resolver.resolve(plain).await.unwrap(),
            ResolvedBody::Plain(_)
        ));
    }
}
