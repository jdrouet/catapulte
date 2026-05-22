use std::collections::HashMap;

use anyhow::Context;
use catapulte_domain::entity::body::{BodySource, MjmlSource, ResolvedBody};
use catapulte_domain::port::template_resolver::{ResolveError, TemplateResolver};

pub struct TemplateResolverAdapter {
    templates: HashMap<String, String>,
    http_client: reqwest::Client,
}

impl TemplateResolverAdapter {
    #[must_use]
    pub fn new(templates: HashMap<String, String>) -> Self {
        Self {
            templates,
            http_client: reqwest::Client::new(),
        }
    }
}

fn resolve_named(templates: &HashMap<String, String>, name: &str) -> Result<String, ResolveError> {
    templates
        .get(name)
        .cloned()
        .ok_or_else(|| ResolveError::NotFound {
            name: name.to_owned(),
        })
}

async fn resolve_remote(client: &reqwest::Client, url: url::Url) -> Result<String, ResolveError> {
    let url_str = url.to_string();
    let response = client
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

async fn resolve_mjml(
    templates: &HashMap<String, String>,
    http_client: &reqwest::Client,
    source: MjmlSource,
) -> Result<String, ResolveError> {
    match source {
        MjmlSource::Inline(s) => Ok(s),
        MjmlSource::Named(name) => resolve_named(templates, &name),
        MjmlSource::Remote(url) => resolve_remote(http_client, url).await,
    }
}

impl TemplateResolver for TemplateResolverAdapter {
    async fn resolve(&self, body: BodySource) -> Result<ResolvedBody, ResolveError> {
        match body {
            BodySource::Plain(plain) => Ok(ResolvedBody::Plain(plain)),
            BodySource::Mjml(source) => resolve_mjml(&self.templates, &self.http_client, source)
                .await
                .map(ResolvedBody::Mjml),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use catapulte_domain::entity::body::{BodySource, MjmlSource, Plain, ResolvedBody};
    use catapulte_domain::port::template_resolver::{ResolveError, TemplateResolver};

    use super::TemplateResolverAdapter;

    #[tokio::test]
    async fn resolve_plain_passthrough() {
        let adapter = TemplateResolverAdapter::new(HashMap::new());
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
        let adapter = TemplateResolverAdapter::new(HashMap::new());
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
        let adapter = TemplateResolverAdapter::new(templates);
        let body = BodySource::Mjml(MjmlSource::Named("welcome".to_owned()));

        let result = adapter.resolve(body).await.unwrap();

        match result {
            ResolvedBody::Mjml(s) => assert_eq!(s, "<mjml/>"),
            ResolvedBody::Plain(_) => panic!("expected Mjml variant"),
        }
    }

    #[tokio::test]
    async fn resolve_named_not_found() {
        let adapter = TemplateResolverAdapter::new(HashMap::new());
        let body = BodySource::Mjml(MjmlSource::Named("missing".to_owned()));

        let result = adapter.resolve(body).await;

        assert!(matches!(
            result,
            Err(ResolveError::NotFound { name }) if name == "missing"
        ));
    }
}
