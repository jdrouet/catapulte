use std::collections::HashSet;
use std::time::Duration;

use anyhow::Context;
use catapulte_domain::port::attachment_fetcher::{AttachmentFetchError, AttachmentFetcher};
use catapulte_domain::port::attachment_store::AttachmentReader;
use futures_util::StreamExt;
use tokio::io::AsyncReadExt;

pub struct HttpAttachmentFetcherConfig {
    pub allowed_domains: HashSet<String>,
    pub allow_http: bool,
    pub max_bytes: u64,
    pub fetch_timeout: Duration,
}

impl Default for HttpAttachmentFetcherConfig {
    fn default() -> Self {
        Self {
            allowed_domains: HashSet::new(),
            allow_http: false,
            max_bytes: 25 * 1024 * 1024,
            fetch_timeout: Duration::from_secs(30),
        }
    }
}

impl HttpAttachmentFetcherConfig {
    /// Reads config from environment variables with the given prefix.
    ///
    /// Variables read:
    /// - `<prefix>_ALLOWED_DOMAINS`: comma-separated list; defaults to empty (all fetches rejected).
    /// - `<prefix>_ALLOW_HTTP`: `"true"` / `"false"`; defaults to `false`.
    /// - `<prefix>_MAX_BYTES`: u64; defaults to 25 MiB.
    /// - `<prefix>_FETCH_TIMEOUT_MS`: u64 milliseconds; defaults to 30 000.
    ///
    /// # Errors
    ///
    /// Returns an error when a present env var cannot be parsed.
    pub fn from_env(prefix: &str) -> anyhow::Result<Self> {
        let allowed_domains = std::env::var(format!("{prefix}_ALLOWED_DOMAINS"))
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect();

        let allow_http = match std::env::var(format!("{prefix}_ALLOW_HTTP"))
            .unwrap_or_default()
            .as_str()
        {
            "true" => true,
            "false" | "" => false,
            other => anyhow::bail!("invalid {prefix}_ALLOW_HTTP value: {other:?}"),
        };

        let max_bytes = match std::env::var(format!("{prefix}_MAX_BYTES")) {
            Ok(v) => v
                .parse::<u64>()
                .with_context(|| format!("invalid {prefix}_MAX_BYTES"))?,
            Err(_) => 25 * 1024 * 1024,
        };

        let fetch_timeout = match std::env::var(format!("{prefix}_FETCH_TIMEOUT_MS")) {
            Ok(v) => {
                let ms = v
                    .parse::<u64>()
                    .with_context(|| format!("invalid {prefix}_FETCH_TIMEOUT_MS"))?;
                Duration::from_millis(ms)
            }
            Err(_) => Duration::from_secs(30),
        };

        Ok(Self {
            allowed_domains,
            allow_http,
            max_bytes,
            fetch_timeout,
        })
    }

    /// Builds an `HttpAttachmentFetcher` from this config.
    ///
    /// # Errors
    ///
    /// Returns an error when the underlying `reqwest::Client` cannot be built.
    pub fn build(self) -> anyhow::Result<HttpAttachmentFetcher> {
        HttpAttachmentFetcher::new(
            self.allowed_domains,
            self.allow_http,
            self.max_bytes,
            self.fetch_timeout,
        )
    }
}

#[derive(Clone)]
pub struct HttpAttachmentFetcher {
    client: reqwest::Client,
    allowed_domains: HashSet<String>,
    allow_http: bool,
    max_bytes: u64,
}

impl HttpAttachmentFetcher {
    fn new(
        allowed_domains: HashSet<String>,
        allow_http: bool,
        max_bytes: u64,
        fetch_timeout: Duration,
    ) -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(fetch_timeout)
            .build()
            .context("building reqwest client for attachment fetcher")?;
        // Normalize at construction: lowercase and strip a single trailing dot so
        // lookups are case-insensitive and FQDN-dot-tolerant.
        let allowed_domains = allowed_domains
            .into_iter()
            .map(|d| d.to_lowercase().trim_end_matches('.').to_owned())
            .collect();
        Ok(Self {
            client,
            allowed_domains,
            allow_http,
            max_bytes,
        })
    }
}

impl AttachmentFetcher for HttpAttachmentFetcher {
    async fn fetch(&self, url: &url::Url) -> Result<AttachmentReader, AttachmentFetchError> {
        // 1. Validate scheme.
        let scheme = url.scheme();
        let ok_scheme = scheme == "https" || (scheme == "http" && self.allow_http);
        if !ok_scheme {
            return Err(AttachmentFetchError::SchemeNotAllowed {
                scheme: scheme.to_owned(),
            });
        }

        // 2. Validate domain (normalize to lowercase, strip trailing FQDN dot).
        let raw_host = url.host_str().unwrap_or("");
        let domain = raw_host.to_lowercase().trim_end_matches('.').to_owned();
        if !self.allowed_domains.contains(&domain) {
            return Err(AttachmentFetchError::DomainNotAllowed {
                domain: raw_host.to_owned(),
            });
        }

        // 3. Fetch.
        let response = self
            .client
            .get(url.clone())
            .send()
            .await
            .context("fetching attachment")
            .map_err(|source| AttachmentFetchError::Fetch { source })?;

        if !response.status().is_success() {
            return Err(AttachmentFetchError::Fetch {
                source: anyhow::anyhow!(
                    "attachment fetch returned non-2xx status: {}",
                    response.status()
                ),
            });
        }

        // 4. Reject early if Content-Length exceeds limit.
        if let Some(cl) = response.content_length()
            && cl > self.max_bytes
        {
            return Err(AttachmentFetchError::TooLarge);
        }

        // 5. Buffer up to max_bytes + 1. We buffer rather than stream so we can
        // detect an oversized body and return a hard error; streaming with an error
        // on overflow would require a custom AsyncRead wrapper.
        let max_bytes = self.max_bytes;
        let stream = response
            .bytes_stream()
            .map(|r| r.map_err(std::io::Error::other));
        let reader = tokio_util::io::StreamReader::new(stream);
        let mut buf = Vec::new();
        reader
            .take(max_bytes + 1)
            .read_to_end(&mut buf)
            .await
            .context("reading attachment body")
            .map_err(|source| AttachmentFetchError::Fetch { source })?;

        if buf.len() as u64 == max_bytes + 1 {
            return Err(AttachmentFetchError::TooLarge);
        }

        Ok(Box::pin(std::io::Cursor::new(buf)))
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::AsyncReadExt;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use catapulte_domain::port::attachment_fetcher::{AttachmentFetchError, AttachmentFetcher};

    use super::{HttpAttachmentFetcher, HttpAttachmentFetcherConfig};

    fn fetcher_for(server: &MockServer) -> HttpAttachmentFetcher {
        let host = server.address().ip().to_string();
        HttpAttachmentFetcherConfig {
            allowed_domains: std::collections::HashSet::from([host]),
            allow_http: true,
            max_bytes: 1024,
            fetch_timeout: std::time::Duration::from_secs(5),
        }
        .build()
        .expect("build fetcher")
    }

    fn url_for(server: &MockServer, path: &str) -> url::Url {
        url::Url::parse(&format!("http://{}{}", server.address(), path)).unwrap()
    }

    #[tokio::test]
    async fn fetch_happy_path_with_allowed_domain_returns_reader_with_body_bytes() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/file.txt"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"hello world".to_vec()))
            .mount(&server)
            .await;

        let fetcher = fetcher_for(&server);
        let url = url_for(&server, "/file.txt");
        let mut reader = fetcher.fetch(&url).await.expect("fetch ok");
        let mut buf = Vec::new();
        reader.read_to_end(&mut buf).await.expect("read");
        assert_eq!(buf, b"hello world");
    }

    #[tokio::test]
    async fn fetch_http_url_rejected_when_allow_http_false() {
        let server = MockServer::start().await;
        let host = server.address().ip().to_string();
        let fetcher = HttpAttachmentFetcherConfig {
            allowed_domains: std::collections::HashSet::from([host.clone()]),
            allow_http: false,
            max_bytes: 1024,
            fetch_timeout: std::time::Duration::from_secs(5),
        }
        .build()
        .expect("build fetcher");

        let url = url_for(&server, "/file.txt");
        let err = fetcher.fetch(&url).await.map(|_| ()).unwrap_err();
        assert!(
            matches!(err, AttachmentFetchError::SchemeNotAllowed { .. }),
            "expected SchemeNotAllowed, got {err:?}"
        );
    }

    #[tokio::test]
    async fn fetch_disallowed_domain_returns_domain_not_allowed() {
        let fetcher = HttpAttachmentFetcherConfig {
            allowed_domains: std::collections::HashSet::new(),
            allow_http: true,
            max_bytes: 1024,
            fetch_timeout: std::time::Duration::from_secs(5),
        }
        .build()
        .expect("build fetcher");

        let url = url::Url::parse("http://example.com/file.txt").unwrap();
        let err = fetcher.fetch(&url).await.map(|_| ()).unwrap_err();
        assert!(
            matches!(err, AttachmentFetchError::DomainNotAllowed { .. }),
            "expected DomainNotAllowed, got {err:?}"
        );
    }

    #[tokio::test]
    async fn fetch_redirect_is_not_followed_returns_fetch_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/redirect"))
            .respond_with(
                ResponseTemplate::new(302).append_header("Location", "http://example.com/other"),
            )
            .mount(&server)
            .await;

        let fetcher = fetcher_for(&server);
        let url = url_for(&server, "/redirect");
        // redirect::none() leaves the 302 as-is; the !is_success() check rejects it.
        let err = fetcher.fetch(&url).await.map(|_| ()).unwrap_err();
        assert!(
            matches!(err, AttachmentFetchError::Fetch { .. }),
            "expected Fetch error for redirect, got {err:?}"
        );
    }

    #[tokio::test]
    async fn fetch_too_large_via_content_length_returns_too_large() {
        let server = MockServer::start().await;
        // Serve a body with Content-Length reporting more than max_bytes (1024).
        Mock::given(method("GET"))
            .and(path("/big"))
            .respond_with(
                ResponseTemplate::new(200)
                    .append_header("Content-Length", "2048")
                    .set_body_bytes(vec![0u8; 2048]),
            )
            .mount(&server)
            .await;

        let fetcher = fetcher_for(&server);
        let url = url_for(&server, "/big");
        let err = fetcher.fetch(&url).await.map(|_| ()).unwrap_err();
        assert!(
            matches!(err, AttachmentFetchError::TooLarge),
            "expected TooLarge, got {err:?}"
        );
    }

    #[tokio::test]
    async fn fetch_response_at_exactly_max_bytes_succeeds() {
        let server = MockServer::start().await;
        // Serve exactly max_bytes (1024) bytes; the early-rejection check passes
        // (not strictly greater) and the body fits within the hard cap.
        let body = vec![0xABu8; 1024];
        Mock::given(method("GET"))
            .and(path("/stream"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body.clone()))
            .mount(&server)
            .await;

        let fetcher = fetcher_for(&server);
        let url = url_for(&server, "/stream");
        let mut reader = fetcher.fetch(&url).await.expect("fetch ok");
        let mut buf = Vec::new();
        reader.read_to_end(&mut buf).await.expect("read");
        assert_eq!(buf.len(), 1024, "expected exactly 1024 bytes");
    }

    #[tokio::test]
    async fn fetch_response_exceeds_max_bytes_returns_too_large() {
        let server = MockServer::start().await;
        // Serve max_bytes + 1 bytes (1025); regardless of which guard fires first
        // (Content-Length pre-check or body-buffering cap), the result must be TooLarge.
        let body = vec![0xCDu8; 1025];
        Mock::given(method("GET"))
            .and(path("/oversized"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
            .mount(&server)
            .await;

        let fetcher = fetcher_for(&server);
        let url = url_for(&server, "/oversized");
        let err = fetcher.fetch(&url).await.map(|_| ()).unwrap_err();
        assert!(
            matches!(err, AttachmentFetchError::TooLarge),
            "expected TooLarge, got {err:?}"
        );
    }

    #[tokio::test]
    async fn fetch_host_match_is_case_insensitive() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/foo"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"ok".to_vec()))
            .mount(&server)
            .await;

        // Allowlist uses the server IP as-is; URL uses the same IP (IPs are already
        // lowercase, but we verify the normalization path doesn't break happy cases).
        // To test case folding on a hostname we build a fetcher with "example.com"
        // and verify the domain check normalises the URL host before lookup.
        let fetcher = HttpAttachmentFetcherConfig {
            allowed_domains: std::collections::HashSet::from(["example.com".to_owned()]),
            allow_http: true,
            max_bytes: 1024,
            fetch_timeout: std::time::Duration::from_secs(5),
        }
        .build()
        .expect("build fetcher");

        // EXAMPLE.COM should match "example.com" in the allowlist.
        let url = url::Url::parse("http://EXAMPLE.com/foo").unwrap();
        // We don't actually hit the network for this check; the domain validation
        // happens before the HTTP request, so a DomainNotAllowed error means the
        // normalization is missing, while any other result (including a Fetch error
        // because there's no server at example.com) means the domain was accepted.
        let result = fetcher.fetch(&url).await;
        assert!(
            !matches!(result, Err(AttachmentFetchError::DomainNotAllowed { .. })),
            "EXAMPLE.com should be accepted when allowlist contains example.com"
        );
    }

    #[tokio::test]
    async fn fetch_host_match_strips_trailing_dot() {
        // Allowlist = {"example.com"}, URL host = "example.com." (FQDN with trailing dot).
        let fetcher = HttpAttachmentFetcherConfig {
            allowed_domains: std::collections::HashSet::from(["example.com".to_owned()]),
            allow_http: true,
            max_bytes: 1024,
            fetch_timeout: std::time::Duration::from_secs(5),
        }
        .build()
        .expect("build fetcher");

        let url = url::Url::parse("http://example.com./foo").unwrap();
        let result = fetcher.fetch(&url).await;
        assert!(
            !matches!(result, Err(AttachmentFetchError::DomainNotAllowed { .. })),
            "example.com. should be accepted when allowlist contains example.com"
        );
    }
}
