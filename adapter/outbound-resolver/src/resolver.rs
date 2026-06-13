use std::collections::{HashMap, HashSet};
use std::env::VarError;
use std::path::PathBuf;

use anyhow::Context;
use catapulte_domain::entity::body::{BodySource, MjmlSource, ResolvedBody};
use catapulte_domain::port::template_resolver::{ResolveError, TemplateResolver};

pub struct ResolverAuthEntry {
    pub host: String,
    pub bearer_token: Option<String>,
    pub headers: Vec<(String, String)>,
}

pub struct TemplateResolverAdapter {
    templates: HashMap<String, String>,
    allowed_domains: HashSet<String>,
    http_client: reqwest::Client,
    auth_headers: HashMap<String, reqwest::header::HeaderMap>,
}

impl TemplateResolverAdapter {
    /// Creates the adapter. Each entry in `auth_entries` attaches auth headers to an exact host;
    /// headers are sent only to requests for that host. A host must also be in `allowed_domains`
    /// to be fetched at all. Duplicate hosts across entries are rejected at build time.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - two entries share the same host
    /// - a bearer token contains characters invalid in an HTTP header value
    /// - a generic header name is invalid
    /// - a generic header value contains characters invalid in an HTTP header value
    /// - both `bearer_token` and an explicit `Authorization` header are configured for the same
    ///   host
    pub fn new(
        templates: HashMap<String, String>,
        allowed_domains: HashSet<String>,
        auth_entries: Vec<ResolverAuthEntry>,
    ) -> anyhow::Result<Self> {
        let http_client = reqwest::Client::builder()
            .build()
            .context("building reqwest client for template resolver")?;

        let mut auth_headers: HashMap<String, reqwest::header::HeaderMap> =
            HashMap::with_capacity(auth_entries.len());
        for entry in auth_entries {
            if auth_headers.contains_key(&entry.host) {
                anyhow::bail!("duplicate host in resolver auth entries: {}", entry.host);
            }
            let mut map = reqwest::header::HeaderMap::new();
            if let Some(ref token) = entry.bearer_token {
                let mut value = reqwest::header::HeaderValue::from_str(&format!("Bearer {token}"))
                    .with_context(|| {
                        format!("invalid resolver bearer token for host {}", entry.host)
                    })?;
                value.set_sensitive(true);
                map.insert(reqwest::header::AUTHORIZATION, value);
            }
            for (name, value) in &entry.headers {
                let header_name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
                    .with_context(|| format!("invalid header name for host {}", entry.host))?;
                if entry.bearer_token.is_some() && header_name == reqwest::header::AUTHORIZATION {
                    anyhow::bail!(
                        "conflicting Authorization header for host {}: both a bearer token and \
                         an explicit authorization header are configured",
                        entry.host
                    );
                }
                let mut header_value =
                    reqwest::header::HeaderValue::from_str(value).with_context(|| {
                        format!(
                            "invalid header value for host {} header {}",
                            entry.host, name
                        )
                    })?;
                header_value.set_sensitive(true);
                map.append(header_name, header_value);
            }
            auth_headers.insert(entry.host, map);
        }

        Ok(Self {
            templates,
            allowed_domains,
            http_client,
            auth_headers,
        })
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
        let host = url.host_str().map(str::to_owned);
        let mut request = self.http_client.get(url);
        if let Some(headers) = host.as_deref().and_then(|h| self.auth_headers.get(h)) {
            request = request.headers(headers.clone());
        }
        let response = request
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
    #[tracing::instrument(skip_all, name = "template.resolve")]
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
        .with_context(|| format!("invalid template filename: {}", path.display()))?
        .to_owned();
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("reading template {}", path.display()))?;
    Ok(Some((name, content)))
}

fn load_templates_from_dir(dir: &std::path::Path) -> anyhow::Result<HashMap<String, String>> {
    std::fs::read_dir(dir)
        .with_context(|| format!("reading templates directory {}", dir.display()))?
        .filter_map(|raw| load_template_entry(raw).transpose())
        .collect()
}

pub struct TemplateResolverConfig {
    pub allowed_domains: HashSet<String>,
    pub templates_dir: Option<PathBuf>,
    pub auth_entries: Vec<ResolverAuthEntry>,
}

impl TemplateResolverConfig {
    /// Reads config from the real environment.
    ///
    /// Reads:
    /// - `{prefix}_ALLOWED_DOMAINS` — comma-separated hostnames allowed for remote fetches
    /// - `{prefix}_TEMPLATES_DIR` — directory of named `.mjml` templates
    /// - `{prefix}_TOKENS` — comma-separated entry names (e.g. `github,gitlab`); absent/empty
    ///   means no auth entries
    /// - `{prefix}_TOKEN_<NAME>_HOST` — exact host for the named entry
    /// - `{prefix}_TOKEN_<NAME>_BEARER_TOKEN` — optional bearer token; sent as
    ///   `Authorization: Bearer <token>` only to the matching host; treated as secret, never logged
    /// - `{prefix}_TOKEN_<NAME>_HEADERS` — comma-separated list of header names to attach to the
    ///   matching host (e.g. `Accept,X-Custom`); absent or empty means no extra headers
    /// - `{prefix}_TOKEN_<NAME>_HEADER_<FRAGMENT>_VALUE` — value for the named header, where
    ///   FRAGMENT is the header name uppercased with `-` replaced by `_` (e.g. `ACCEPT`,
    ///   `PRIVATE_TOKEN`); treated as secret, never logged
    ///
    /// Each entry listed in `_TOKENS` must configure at least a bearer token or one header;
    /// a host-only entry with no auth is rejected.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - a named entry is missing its `_HOST`
    /// - a named entry has neither a bearer token nor any headers
    /// - a header listed in `_HEADERS` is missing its `_VALUE` variable
    /// - two header names in `_HEADERS` map to the same FRAGMENT (would silently shadow)
    pub fn from_env(prefix: &str) -> anyhow::Result<Self> {
        Self::from_lookup(prefix, |key| std::env::var(key))
    }

    fn from_lookup<F>(prefix: &str, lookup: F) -> anyhow::Result<Self>
    where
        F: Fn(&str) -> Result<String, VarError>,
    {
        let allowed_domains = lookup(&format!("{prefix}_ALLOWED_DOMAINS"))
            .ok()
            .map(|v| {
                v.split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();

        let templates_dir = lookup(&format!("{prefix}_TEMPLATES_DIR"))
            .ok()
            .map(PathBuf::from);

        let auth_entries = lookup(&format!("{prefix}_TOKENS"))
            .ok()
            .map(|v| {
                v.split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(|name| {
                        let upper = name.to_uppercase();
                        let host_key = format!("{prefix}_TOKEN_{upper}_HOST");
                        let token_key = format!("{prefix}_TOKEN_{upper}_BEARER_TOKEN");
                        let headers_key = format!("{prefix}_TOKEN_{upper}_HEADERS");

                        let host = lookup(&host_key)
                            .ok()
                            .map(|v| v.trim().to_owned())
                            .filter(|s| !s.is_empty())
                            .with_context(|| format!("missing or empty env var {host_key}"))?;

                        let bearer_token = lookup(&token_key)
                            .ok()
                            .map(|v| v.trim().to_owned())
                            .filter(|s| !s.is_empty());

                        let mut seen_fragments: HashMap<String, String> = HashMap::new();
                        let headers = lookup(&headers_key)
                            .ok()
                            .map(|v| {
                                v.split(',')
                                    .map(str::trim)
                                    .filter(|s| !s.is_empty())
                                    .map(|header_name| {
                                        let fragment = header_name.to_uppercase().replace('-', "_");
                                        if let Some(prior) = seen_fragments
                                            .insert(fragment.clone(), header_name.to_owned())
                                        {
                                            anyhow::bail!(
                                                "header names {prior} and {header_name} for \
                                                 entry {name} both map to fragment {fragment}"
                                            );
                                        }
                                        let value_key = format!(
                                            "{prefix}_TOKEN_{upper}_HEADER_{fragment}_VALUE"
                                        );
                                        let value = lookup(&value_key)
                                            .ok()
                                            .map(|v| v.trim().to_owned())
                                            .filter(|s| !s.is_empty())
                                            .with_context(|| {
                                                format!("missing or empty env var {value_key}")
                                            })?;
                                        Ok((header_name.to_owned(), value))
                                    })
                                    .collect::<anyhow::Result<Vec<_>>>()
                            })
                            .transpose()?
                            .unwrap_or_default();

                        if bearer_token.is_none() && headers.is_empty() {
                            anyhow::bail!(
                                "auth entry {name} has a host but no bearer token or headers"
                            );
                        }

                        Ok(ResolverAuthEntry {
                            host,
                            bearer_token,
                            headers,
                        })
                    })
                    .collect::<anyhow::Result<Vec<_>>>()
            })
            .transpose()?
            .unwrap_or_default();

        Ok(Self {
            allowed_domains,
            templates_dir,
            auth_entries,
        })
    }

    /// # Errors
    ///
    /// Returns an error if the templates directory cannot be read, any bearer token is invalid,
    /// any custom header name or value is invalid, a bearer token is combined with an explicit
    /// `Authorization` header for the same host, or two entries share the same host.
    pub fn build(self) -> anyhow::Result<TemplateResolverAdapter> {
        let templates = match self.templates_dir {
            None => HashMap::new(),
            Some(ref dir) => load_templates_from_dir(dir)?,
        };
        TemplateResolverAdapter::new(templates, self.allowed_domains, self.auth_entries)
            .context("building template resolver adapter")
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};
    use std::env::VarError;
    use std::path::PathBuf;

    use catapulte_domain::entity::body::{BodySource, MjmlSource, Plain, ResolvedBody};
    use catapulte_domain::port::template_resolver::{ResolveError, TemplateResolver};

    use super::{ResolverAuthEntry, TemplateResolverAdapter, TemplateResolverConfig};

    fn make_lookup(
        vars: HashMap<&'static str, &'static str>,
    ) -> impl Fn(&str) -> Result<String, VarError> {
        move |key: &str| {
            vars.get(key)
                .map(|v| (*v).to_owned())
                .ok_or(VarError::NotPresent)
        }
    }

    #[tokio::test]
    async fn resolve_plain_passthrough() {
        let adapter =
            TemplateResolverAdapter::new(HashMap::new(), HashSet::new(), Vec::new()).unwrap();
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
        let adapter =
            TemplateResolverAdapter::new(HashMap::new(), HashSet::new(), Vec::new()).unwrap();
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
        let adapter = TemplateResolverAdapter::new(templates, HashSet::new(), Vec::new()).unwrap();
        let body = BodySource::Mjml(MjmlSource::Named("welcome".to_owned()));

        let result = adapter.resolve(body).await.unwrap();

        match result {
            ResolvedBody::Mjml(s) => assert_eq!(s, "<mjml/>"),
            ResolvedBody::Plain(_) => panic!("expected Mjml variant"),
        }
    }

    #[tokio::test]
    async fn resolve_named_not_found() {
        let adapter =
            TemplateResolverAdapter::new(HashMap::new(), HashSet::new(), Vec::new()).unwrap();
        let body = BodySource::Mjml(MjmlSource::Named("missing".to_owned()));

        let result = adapter.resolve(body).await;

        assert!(matches!(
            result,
            Err(ResolveError::NotFound { name }) if name == "missing"
        ));
    }

    #[tokio::test]
    async fn resolve_remote_domain_not_in_whitelist_returns_error() {
        let adapter =
            TemplateResolverAdapter::new(HashMap::new(), HashSet::new(), Vec::new()).unwrap();
        let url = url::Url::parse("https://example.com/template.mjml").unwrap();
        let body = BodySource::Mjml(MjmlSource::Remote(url));

        let result = adapter.resolve(body).await;

        assert!(matches!(result, Err(ResolveError::DomainNotAllowed { .. })));
    }

    #[tokio::test]
    async fn resolve_remote_domain_in_whitelist_proceeds() {
        let mut allowed = HashSet::new();
        allowed.insert("example.com".to_owned());
        let adapter = TemplateResolverAdapter::new(HashMap::new(), allowed, Vec::new()).unwrap();
        let url = url::Url::parse("https://example.com/template.mjml").unwrap();
        let body = BodySource::Mjml(MjmlSource::Remote(url));

        let result = adapter.resolve(body).await;

        assert!(
            !matches!(result, Err(ResolveError::DomainNotAllowed { .. })),
            "expected whitelist check to pass, got DomainNotAllowed"
        );
    }

    #[test]
    fn config_from_lookup_defaults_to_empty() {
        let vars = HashMap::new();
        let config =
            TemplateResolverConfig::from_lookup("RESOLVER_TEST_EMPTY", make_lookup(vars)).unwrap();
        assert!(config.allowed_domains.is_empty());
        assert!(config.templates_dir.is_none());
        assert!(config.auth_entries.is_empty());
    }

    #[test]
    fn config_from_lookup_parses_two_entries() {
        let mut vars = HashMap::new();
        vars.insert("MYPREFIX_TOKENS", "github,gitlab");
        vars.insert("MYPREFIX_TOKEN_GITHUB_HOST", "raw.githubusercontent.com");
        vars.insert("MYPREFIX_TOKEN_GITHUB_BEARER_TOKEN", "ghp_xxx");
        vars.insert("MYPREFIX_TOKEN_GITLAB_HOST", "gitlab.com");
        vars.insert("MYPREFIX_TOKEN_GITLAB_BEARER_TOKEN", "glpat_yyy");
        let config = TemplateResolverConfig::from_lookup("MYPREFIX", make_lookup(vars)).unwrap();
        assert_eq!(config.auth_entries.len(), 2);
        assert_eq!(config.auth_entries[0].host, "raw.githubusercontent.com");
        assert_eq!(
            config.auth_entries[0].bearer_token,
            Some("ghp_xxx".to_owned())
        );
        assert!(config.auth_entries[0].headers.is_empty());
        assert_eq!(config.auth_entries[1].host, "gitlab.com");
        assert_eq!(
            config.auth_entries[1].bearer_token,
            Some("glpat_yyy".to_owned())
        );
        assert!(config.auth_entries[1].headers.is_empty());
    }

    #[test]
    fn config_from_lookup_missing_host_errors() {
        let mut vars = HashMap::new();
        vars.insert("MYPREFIX_TOKENS", "github");
        vars.insert("MYPREFIX_TOKEN_GITHUB_BEARER_TOKEN", "ghp_xxx");
        // _HOST intentionally absent
        let result = TemplateResolverConfig::from_lookup("MYPREFIX", make_lookup(vars));
        assert!(result.is_err());
    }

    #[test]
    fn config_from_lookup_host_with_no_auth_errors() {
        let mut vars = HashMap::new();
        vars.insert("MYPREFIX_TOKENS", "github");
        vars.insert("MYPREFIX_TOKEN_GITHUB_HOST", "raw.githubusercontent.com");
        // neither _BEARER_TOKEN nor _HEADERS, should be rejected
        let result = TemplateResolverConfig::from_lookup("MYPREFIX", make_lookup(vars));
        assert!(result.is_err());
    }

    #[test]
    fn config_from_lookup_bearer_and_header() {
        let mut vars = HashMap::new();
        vars.insert("MYPREFIX_TOKENS", "github");
        vars.insert("MYPREFIX_TOKEN_GITHUB_HOST", "api.github.com");
        vars.insert("MYPREFIX_TOKEN_GITHUB_BEARER_TOKEN", "ghp_xxx");
        vars.insert("MYPREFIX_TOKEN_GITHUB_HEADERS", "Accept");
        vars.insert(
            "MYPREFIX_TOKEN_GITHUB_HEADER_ACCEPT_VALUE",
            "application/vnd.github+json",
        );
        let config = TemplateResolverConfig::from_lookup("MYPREFIX", make_lookup(vars)).unwrap();
        assert_eq!(config.auth_entries.len(), 1);
        let entry = &config.auth_entries[0];
        assert_eq!(entry.host, "api.github.com");
        assert_eq!(entry.bearer_token, Some("ghp_xxx".to_owned()));
        assert_eq!(entry.headers.len(), 1);
        assert_eq!(entry.headers[0].0, "Accept");
        assert_eq!(entry.headers[0].1, "application/vnd.github+json");
    }

    #[test]
    fn config_from_lookup_header_only_no_bearer() {
        let mut vars = HashMap::new();
        vars.insert("MYPREFIX_TOKENS", "gitlab");
        vars.insert("MYPREFIX_TOKEN_GITLAB_HOST", "gitlab.com");
        vars.insert("MYPREFIX_TOKEN_GITLAB_HEADERS", "PRIVATE-TOKEN");
        vars.insert(
            "MYPREFIX_TOKEN_GITLAB_HEADER_PRIVATE_TOKEN_VALUE",
            "glpat_zzz",
        );
        let config = TemplateResolverConfig::from_lookup("MYPREFIX", make_lookup(vars)).unwrap();
        assert_eq!(config.auth_entries.len(), 1);
        let entry = &config.auth_entries[0];
        assert_eq!(entry.host, "gitlab.com");
        assert!(entry.bearer_token.is_none());
        assert_eq!(entry.headers.len(), 1);
        assert_eq!(entry.headers[0].0, "PRIVATE-TOKEN");
        assert_eq!(entry.headers[0].1, "glpat_zzz");
    }

    #[test]
    fn config_from_lookup_header_listed_but_value_missing_errors() {
        let mut vars = HashMap::new();
        vars.insert("MYPREFIX_TOKENS", "github");
        vars.insert("MYPREFIX_TOKEN_GITHUB_HOST", "api.github.com");
        vars.insert("MYPREFIX_TOKEN_GITHUB_HEADERS", "Accept");
        // _HEADER_ACCEPT_VALUE intentionally absent
        let result = TemplateResolverConfig::from_lookup("MYPREFIX", make_lookup(vars));
        assert!(result.is_err());
    }

    #[test]
    fn config_from_lookup_fragment_collision_errors() {
        let mut vars = HashMap::new();
        vars.insert("MYPREFIX_TOKENS", "github");
        vars.insert("MYPREFIX_TOKEN_GITHUB_HOST", "api.github.com");
        // "X-Foo" and "X_Foo" both produce fragment "X_FOO"
        vars.insert("MYPREFIX_TOKEN_GITHUB_HEADERS", "X-Foo,X_Foo");
        vars.insert("MYPREFIX_TOKEN_GITHUB_HEADER_X_FOO_VALUE", "bar");
        let result = TemplateResolverConfig::from_lookup("MYPREFIX", make_lookup(vars));
        assert!(result.is_err());
    }

    #[test]
    fn new_with_duplicate_host_returns_err() {
        let entries = vec![
            ResolverAuthEntry {
                host: "example.com".to_owned(),
                bearer_token: Some("tok-a".to_owned()),
                headers: Vec::new(),
            },
            ResolverAuthEntry {
                host: "example.com".to_owned(),
                bearer_token: Some("tok-b".to_owned()),
                headers: Vec::new(),
            },
        ];
        let result = TemplateResolverAdapter::new(HashMap::new(), HashSet::new(), entries);
        assert!(result.is_err());
    }

    #[test]
    fn new_with_invalid_token_returns_err() {
        let entries = vec![ResolverAuthEntry {
            host: "example.com".to_owned(),
            bearer_token: Some("bad\nvalue".to_owned()),
            headers: Vec::new(),
        }];
        let result = TemplateResolverAdapter::new(HashMap::new(), HashSet::new(), entries);
        assert!(result.is_err());
    }

    #[test]
    fn new_with_invalid_header_name_returns_err() {
        let entries = vec![ResolverAuthEntry {
            host: "example.com".to_owned(),
            bearer_token: None,
            headers: vec![("bad header name".to_owned(), "value".to_owned())],
        }];
        let result = TemplateResolverAdapter::new(HashMap::new(), HashSet::new(), entries);
        assert!(result.is_err());
    }

    #[test]
    fn new_with_invalid_header_value_returns_err() {
        let entries = vec![ResolverAuthEntry {
            host: "example.com".to_owned(),
            bearer_token: None,
            headers: vec![("X-Custom".to_owned(), "bad\nvalue".to_owned())],
        }];
        let result = TemplateResolverAdapter::new(HashMap::new(), HashSet::new(), entries);
        assert!(result.is_err());
    }

    #[test]
    fn new_with_bearer_and_explicit_authorization_header_conflicts() {
        let entries = vec![ResolverAuthEntry {
            host: "example.com".to_owned(),
            bearer_token: Some("tok-a".to_owned()),
            headers: vec![("Authorization".to_owned(), "Bearer other".to_owned())],
        }];
        let Err(err) = TemplateResolverAdapter::new(HashMap::new(), HashSet::new(), entries) else {
            panic!("expected conflict error but got Ok")
        };
        let msg = format!("{err:#}");
        assert!(
            msg.contains("conflicting Authorization header"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn new_no_entries_builds_ok() {
        let result = TemplateResolverAdapter::new(HashMap::new(), HashSet::new(), Vec::new());
        assert!(result.is_ok());
    }

    #[test]
    fn config_build_no_dir_returns_adapter() {
        let config = TemplateResolverConfig {
            allowed_domains: HashSet::new(),
            templates_dir: None,
            auth_entries: Vec::new(),
        };
        assert!(config.build().is_ok());
    }

    #[test]
    fn config_build_invalid_dir_returns_error() {
        let config = TemplateResolverConfig {
            allowed_domains: HashSet::new(),
            templates_dir: Some(PathBuf::from("/nonexistent/resolver/templates")),
            auth_entries: Vec::new(),
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
            auth_entries: Vec::new(),
        };
        let adapter = config.build().unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let body = BodySource::Mjml(MjmlSource::Named("welcome".to_owned()));
        let result = rt.block_on(adapter.resolve(body)).unwrap();
        match result {
            ResolvedBody::Mjml(s) => assert_eq!(s, "<mjml/>"),
            ResolvedBody::Plain(_) => panic!("expected Mjml variant"),
        }
    }

    #[tokio::test]
    async fn per_host_token_sent_correct_token_other_not_leaked() {
        use wiremock::matchers::{header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let host = server.address().ip().to_string();
        let port = server.address().port();

        Mock::given(method("GET"))
            .and(path("/template.mjml"))
            .and(header("authorization", "Bearer tok-a"))
            .respond_with(ResponseTemplate::new(200).set_body_string("<mjml/>"))
            .expect(1)
            .mount(&server)
            .await;

        // Guard: the other entry's token must never reach this host.
        Mock::given(method("GET"))
            .and(path("/template.mjml"))
            .and(header("authorization", "Bearer tok-b"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;

        let mut allowed = HashSet::new();
        allowed.insert(host.clone());

        let entries = vec![
            ResolverAuthEntry {
                host: host.clone(),
                bearer_token: Some("tok-a".to_owned()),
                headers: Vec::new(),
            },
            ResolverAuthEntry {
                host: "other.example.invalid".to_owned(),
                bearer_token: Some("tok-b".to_owned()),
                headers: Vec::new(),
            },
        ];
        let adapter = TemplateResolverAdapter::new(HashMap::new(), allowed, entries).unwrap();

        let url = url::Url::parse(&format!("http://{host}:{port}/template.mjml")).unwrap();
        let body = BodySource::Mjml(MjmlSource::Remote(url));
        let result = adapter.resolve(body).await.unwrap();

        match result {
            ResolvedBody::Mjml(s) => assert_eq!(s, "<mjml/>"),
            ResolvedBody::Plain(_) => panic!("expected Mjml variant"),
        }
    }

    #[tokio::test]
    async fn generic_header_attached_to_matching_host() {
        use wiremock::matchers::{header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let host = server.address().ip().to_string();
        let port = server.address().port();

        Mock::given(method("GET"))
            .and(path("/template.mjml"))
            .and(header("X-Custom-Header", "custom-value"))
            .respond_with(ResponseTemplate::new(200).set_body_string("<mjml/>"))
            .expect(1)
            .mount(&server)
            .await;

        let mut allowed = HashSet::new();
        allowed.insert(host.clone());

        let entries = vec![ResolverAuthEntry {
            host: host.clone(),
            bearer_token: None,
            headers: vec![("X-Custom-Header".to_owned(), "custom-value".to_owned())],
        }];
        let adapter = TemplateResolverAdapter::new(HashMap::new(), allowed, entries).unwrap();

        let url = url::Url::parse(&format!("http://{host}:{port}/template.mjml")).unwrap();
        let body = BodySource::Mjml(MjmlSource::Remote(url));
        let result = adapter.resolve(body).await.unwrap();

        match result {
            ResolvedBody::Mjml(s) => assert_eq!(s, "<mjml/>"),
            ResolvedBody::Plain(_) => panic!("expected Mjml variant"),
        }
    }
}
