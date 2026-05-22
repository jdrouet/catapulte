use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use anyhow::Context;
use catapulte_domain::entity::body::{BodySource, MjmlSource, ResolvedBody};
use catapulte_domain::port::template_resolver::{ResolveError, TemplateResolver};

pub struct TemplateResolverAdapter {
    templates: HashMap<String, String>,
    allowed_domains: HashSet<String>,
    http_client: reqwest::Client,
}

impl TemplateResolverAdapter {
    #[must_use]
    pub fn new(templates: HashMap<String, String>, allowed_domains: HashSet<String>) -> Self {
        Self {
            templates,
            allowed_domains,
            http_client: reqwest::Client::new(),
        }
    }

    fn check_domain(&self, url: &url::Url) -> Result<(), ResolveError> {
        let host = url.host_str().unwrap_or("");
        if self.allowed_domains.contains(host) {
            Ok(())
        } else {
            Err(ResolveError::DomainNotAllowed {
                url: url.to_string(),
            })
        }
    }

    async fn resolve_remote(&self, url: url::Url) -> Result<String, ResolveError> {
        let url_str = url.to_string();
        let response = self
            .http_client
            .get(url)
            .send()
            .await
            .context("http request failed")
            .map_err(|source| ResolveError::Fetch {
                url: url_str.clone(),
                source,
            })?
            .error_for_status()
            .context("http error response")
            .map_err(|source| ResolveError::Fetch {
                url: url_str.clone(),
                source,
            })?;
        response
            .text()
            .await
            .context("reading response body")
            .map_err(|source| ResolveError::Fetch {
                url: url_str,
                source,
            })
    }

    async fn resolve_mjml(&self, source: MjmlSource) -> Result<String, ResolveError> {
        match source {
            MjmlSource::Inline(s) => Ok(s),
            MjmlSource::Named(name) => self
                .templates
                .get(&name)
                .cloned()
                .ok_or_else(|| ResolveError::NotFound { name }),
            MjmlSource::Remote(url) => {
                self.check_domain(&url)?;
                self.resolve_remote(url).await
            }
        }
    }
}

impl TemplateResolver for TemplateResolverAdapter {
    async fn resolve(&self, body: BodySource) -> Result<ResolvedBody, ResolveError> {
        match body {
            BodySource::Plain(plain) => Ok(ResolvedBody::Plain(plain)),
            BodySource::Mjml(source) => self.resolve_mjml(source).await.map(ResolvedBody::Mjml),
        }
    }
}

fn load_template_entry(
    raw: std::io::Result<std::fs::DirEntry>,
) -> anyhow::Result<Option<(String, String)>> {
    let entry = raw.context("reading directory entry")?;
    let path = entry.path();
    if path.extension().and_then(|e| e.to_str()) != Some("mjml") {
        return Ok(None);
    }
    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .with_context(|| format!("invalid template filename: {path:?}"))?
        .to_owned();
    let content =
        std::fs::read_to_string(&path).with_context(|| format!("reading template {path:?}"))?;
    Ok(Some((name, content)))
}

fn load_templates_from_dir(dir: &std::path::Path) -> anyhow::Result<HashMap<String, String>> {
    std::fs::read_dir(dir)
        .with_context(|| format!("reading templates directory {dir:?}"))?
        .filter_map(|raw| load_template_entry(raw).transpose())
        .collect()
}

pub struct TemplateResolverConfig {
    pub allowed_domains: HashSet<String>,
    pub templates_dir: Option<PathBuf>,
}

impl TemplateResolverConfig {
    /// # Errors
    ///
    /// Never fails; env vars are optional and defaults are used when absent.
    pub fn from_env(prefix: &str) -> anyhow::Result<Self> {
        let allowed_domains = std::env::var(format!("{prefix}_ALLOWED_DOMAINS"))
            .ok()
            .map(|v| {
                v.split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        let templates_dir = std::env::var(format!("{prefix}_TEMPLATES_DIR"))
            .ok()
            .map(PathBuf::from);
        Ok(Self {
            allowed_domains,
            templates_dir,
        })
    }

    /// # Errors
    ///
    /// Returns an error if the templates directory cannot be read.
    pub fn build(self) -> anyhow::Result<TemplateResolverAdapter> {
        let templates = match self.templates_dir {
            None => HashMap::new(),
            Some(ref dir) => load_templates_from_dir(dir)?,
        };
        Ok(TemplateResolverAdapter::new(
            templates,
            self.allowed_domains,
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};
    use std::path::PathBuf;

    use catapulte_domain::entity::body::{BodySource, MjmlSource, Plain, ResolvedBody};
    use catapulte_domain::port::template_resolver::{ResolveError, TemplateResolver};

    use super::{TemplateResolverAdapter, TemplateResolverConfig};

    #[tokio::test]
    async fn resolve_plain_passthrough() {
        let adapter = TemplateResolverAdapter::new(HashMap::new(), HashSet::new());
        let plain = Plain::try_new(Some("hello".to_owned()), None).unwrap();
        let body = BodySource::Plain(plain);

        let result = adapter.resolve(body).await.unwrap();

        match result {
            ResolvedBody::Plain(p) => {
                assert_eq!(p.text(), Some("hello"));
                assert_eq!(p.html(), None);
            }
            ResolvedBody::Mjml(_) => panic!("expected Plain variant"),
        }
    }

    #[tokio::test]
    async fn resolve_mjml_inline_passthrough() {
        let adapter = TemplateResolverAdapter::new(HashMap::new(), HashSet::new());
        let body = BodySource::Mjml(MjmlSource::Inline("source".to_owned()));

        let result = adapter.resolve(body).await.unwrap();

        match result {
            ResolvedBody::Mjml(s) => assert_eq!(s, "source"),
            ResolvedBody::Plain(_) => panic!("expected Mjml variant"),
        }
    }

    #[tokio::test]
    async fn resolve_named_found() {
        let mut templates = HashMap::new();
        templates.insert("welcome".to_owned(), "<mjml/>".to_owned());
        let adapter = TemplateResolverAdapter::new(templates, HashSet::new());
        let body = BodySource::Mjml(MjmlSource::Named("welcome".to_owned()));

        let result = adapter.resolve(body).await.unwrap();

        match result {
            ResolvedBody::Mjml(s) => assert_eq!(s, "<mjml/>"),
            ResolvedBody::Plain(_) => panic!("expected Mjml variant"),
        }
    }

    #[tokio::test]
    async fn resolve_named_not_found() {
        let adapter = TemplateResolverAdapter::new(HashMap::new(), HashSet::new());
        let body = BodySource::Mjml(MjmlSource::Named("missing".to_owned()));

        let result = adapter.resolve(body).await;

        assert!(matches!(
            result,
            Err(ResolveError::NotFound { name }) if name == "missing"
        ));
    }

    #[tokio::test]
    async fn resolve_remote_domain_not_in_whitelist_returns_error() {
        let adapter = TemplateResolverAdapter::new(HashMap::new(), HashSet::new());
        let url = url::Url::parse("https://example.com/template.mjml").unwrap();
        let body = BodySource::Mjml(MjmlSource::Remote(url));

        let result = adapter.resolve(body).await;

        assert!(matches!(result, Err(ResolveError::DomainNotAllowed { .. })));
    }

    #[tokio::test]
    async fn resolve_remote_domain_in_whitelist_proceeds() {
        let mut allowed = HashSet::new();
        allowed.insert("example.com".to_owned());
        let adapter = TemplateResolverAdapter::new(HashMap::new(), allowed);
        let url = url::Url::parse("https://example.com/template.mjml").unwrap();
        let body = BodySource::Mjml(MjmlSource::Remote(url));

        let result = adapter.resolve(body).await;

        assert!(
            !matches!(result, Err(ResolveError::DomainNotAllowed { .. })),
            "expected whitelist check to pass, got DomainNotAllowed"
        );
    }

    #[test]
    fn config_from_env_defaults_to_empty() {
        let config = TemplateResolverConfig::from_env("RESOLVER_TEST_EMPTY").unwrap();
        assert!(config.allowed_domains.is_empty());
        assert!(config.templates_dir.is_none());
    }

    #[test]
    fn config_build_no_dir_returns_adapter() {
        let config = TemplateResolverConfig {
            allowed_domains: HashSet::new(),
            templates_dir: None,
        };
        assert!(config.build().is_ok());
    }

    #[test]
    fn config_build_invalid_dir_returns_error() {
        let config = TemplateResolverConfig {
            allowed_domains: HashSet::new(),
            templates_dir: Some(PathBuf::from("/nonexistent/resolver/templates")),
        };
        assert!(config.build().is_err());
    }

    #[test]
    fn config_build_with_dir_loads_mjml_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("welcome.mjml"), "<mjml/>").unwrap();
        std::fs::write(dir.path().join("ignored.txt"), "not a template").unwrap();
        let config = TemplateResolverConfig {
            allowed_domains: HashSet::new(),
            templates_dir: Some(dir.path().to_owned()),
        };
        let adapter = config.build().unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let body = BodySource::Mjml(MjmlSource::Named("welcome".to_owned()));
        let result = rt.block_on(adapter.resolve(body)).unwrap();
        match result {
            ResolvedBody::Mjml(s) => assert_eq!(s, "<mjml/>"),
            _ => panic!("expected Mjml variant"),
        }
    }
}
