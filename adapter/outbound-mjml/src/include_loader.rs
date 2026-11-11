use std::collections::HashSet;
use std::env::VarError;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use mrml::prelude::parser::loader::{AsyncIncludeLoader, IncludeLoader, IncludeLoaderError};
use mrml::prelude::parser::local_loader::LocalIncludeLoader;
use mrml::prelude::parser::multi_loader::MultiIncludeLoaderAsync;
use mrml::prelude::parser::noop_loader::NoopIncludeLoader;

#[derive(Debug)]
struct SpawnBlockingLocalLoader {
    inner: Arc<LocalIncludeLoader>,
}

impl SpawnBlockingLocalLoader {
    fn new(root: PathBuf) -> Self {
        Self {
            inner: Arc::new(LocalIncludeLoader::new(root)),
        }
    }
}

#[async_trait::async_trait]
impl AsyncIncludeLoader for SpawnBlockingLocalLoader {
    async fn async_resolve(&self, path: &str) -> Result<String, IncludeLoaderError> {
        let inner = Arc::clone(&self.inner);
        let path_owned = path.to_owned();
        let join = tokio::task::spawn_blocking(move || inner.resolve(&path_owned)).await;
        match join {
            Ok(result) => result,
            Err(_) => Err(IncludeLoaderError::new(path, std::io::ErrorKind::Other)
                .with_message("local include task panicked")),
        }
    }
}

const DEFAULT_HTTP_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_HTTP_MAX_BYTES: u64 = 1_048_576;

#[derive(Clone, Debug)]
struct GuardedHttpLoader {
    client: reqwest::Client,
    allowed: Arc<HashSet<String>>,
    max_bytes: u64,
}

impl GuardedHttpLoader {
    fn new(allowed: HashSet<String>, timeout: Duration, max_bytes: u64) -> anyhow::Result<Self> {
        // Redirects stay off so an allowed origin cannot bounce a fetch to another host.
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(timeout)
            .build()
            .context("building reqwest client for include loader")?;
        Ok(Self {
            client,
            allowed: Arc::new(allowed),
            max_bytes,
        })
    }
}

fn fetch_error(path: &str, message: &'static str) -> IncludeLoaderError {
    IncludeLoaderError::new(path, std::io::ErrorKind::Other).with_message(message)
}

#[async_trait::async_trait]
impl AsyncIncludeLoader for GuardedHttpLoader {
    async fn async_resolve(&self, path: &str) -> Result<String, IncludeLoaderError> {
        let url = url::Url::parse(path)
            .map_err(|_| IncludeLoaderError::not_found(path).with_message("invalid include url"))?;
        if !self.allowed.contains(&url.origin().ascii_serialization()) {
            return Err(
                IncludeLoaderError::new(path, std::io::ErrorKind::PermissionDenied)
                    .with_message("include origin is not allowed"),
            );
        }
        let mut response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|_| fetch_error(path, "include request failed"))?;
        if !response.status().is_success() {
            return Err(fetch_error(
                path,
                "include request returned a non-2xx status",
            ));
        }
        if response
            .content_length()
            .is_some_and(|n| n > self.max_bytes)
        {
            return Err(fetch_error(path, "include response is too large"));
        }
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| fetch_error(path, "reading include response failed"))?
        {
            if body.len() as u64 + chunk.len() as u64 > self.max_bytes {
                return Err(fetch_error(path, "include response is too large"));
            }
            body.extend_from_slice(&chunk);
        }
        String::from_utf8(body).map_err(|_| fetch_error(path, "include response is not utf-8"))
    }
}

fn parse_positive<F>(lookup: &F, key: &str, default: u64) -> anyhow::Result<u64>
where
    F: Fn(&str) -> Result<String, VarError>,
{
    let raw = match lookup(key) {
        Ok(raw) => raw,
        Err(VarError::NotPresent) => return Ok(default),
        Err(VarError::NotUnicode(_)) => anyhow::bail!("env var {key} is not valid unicode"),
    };
    match raw.trim().parse::<u64>() {
        Ok(n) if n > 0 => Ok(n),
        _ => anyhow::bail!("env var {key} must be a positive integer, got {raw:?}"),
    }
}

pub struct IncludeLoaderConfig {
    pub fs_root: Option<PathBuf>,
    http_allow: Option<HashSet<String>>,
    http_timeout: Duration,
    http_max_bytes: u64,
}

impl Default for IncludeLoaderConfig {
    fn default() -> Self {
        Self {
            fs_root: None,
            http_allow: None,
            http_timeout: DEFAULT_HTTP_TIMEOUT,
            http_max_bytes: DEFAULT_HTTP_MAX_BYTES,
        }
    }
}

impl IncludeLoaderConfig {
    /// Reads:
    /// - `<prefix>_FS_ROOT`          (path, optional)
    /// - `<prefix>_HTTP_ALLOW`       (comma-separated origins, optional; empty disables HTTP includes)
    /// - `<prefix>_HTTP_TIMEOUT_MS`  (total timeout per fetch, default 30000)
    /// - `<prefix>_HTTP_MAX_BYTES`   (maximum include size, default 1048576)
    ///
    /// # Errors
    ///
    /// Returns an error if `HTTP_DENY` is set (the mode was removed), an origin is invalid, or
    /// a numeric variable is zero, invalid or not valid unicode.
    pub fn from_env(prefix: &str) -> anyhow::Result<Self> {
        Self::from_lookup(prefix, |key| std::env::var(key))
    }

    pub(crate) fn from_lookup<F>(prefix: &str, lookup: F) -> anyhow::Result<Self>
    where
        F: Fn(&str) -> Result<String, VarError>,
    {
        let fs_root = if let Some(raw) = lookup(&format!("{prefix}_FS_ROOT"))
            .ok()
            .filter(|s| !s.is_empty())
        {
            let path = PathBuf::from(&raw);
            let canonical = path
                .canonicalize()
                .with_context(|| format!("canonicalizing {prefix}_FS_ROOT={raw}"))?;
            Some(canonical)
        } else {
            None
        };

        let deny_set = match lookup(&format!("{prefix}_HTTP_DENY")) {
            Ok(v) => !v.is_empty(),
            Err(VarError::NotPresent) => false,
            Err(VarError::NotUnicode(_)) => true,
        };
        if deny_set {
            anyhow::bail!(
                "{prefix}_HTTP_DENY is no longer supported; list allowed origins in \
                 {prefix}_HTTP_ALLOW instead"
            );
        }

        let http_allow = parse_origin_set(
            prefix,
            "HTTP_ALLOW",
            lookup(&format!("{prefix}_HTTP_ALLOW")).ok(),
        )?;
        let http_timeout = Duration::from_millis(parse_positive(
            &lookup,
            &format!("{prefix}_HTTP_TIMEOUT_MS"),
            30_000,
        )?);
        let http_max_bytes = parse_positive(
            &lookup,
            &format!("{prefix}_HTTP_MAX_BYTES"),
            DEFAULT_HTTP_MAX_BYTES,
        )?;

        Ok(Self {
            fs_root,
            http_allow,
            http_timeout,
            http_max_bytes,
        })
    }

    /// # Errors
    ///
    /// Returns an error if the HTTP client cannot be built.
    pub fn build(self) -> anyhow::Result<Box<dyn AsyncIncludeLoader + Send + Sync>> {
        let fs: Option<SpawnBlockingLocalLoader> = self.fs_root.map(SpawnBlockingLocalLoader::new);
        let http = self
            .http_allow
            .map(|allowed| GuardedHttpLoader::new(allowed, self.http_timeout, self.http_max_bytes))
            .transpose()?;

        let loader: Box<dyn AsyncIncludeLoader + Send + Sync> = match (fs, http) {
            (None, None) => Box::new(NoopIncludeLoader),
            (Some(fs), None) => Box::new(fs),
            (fs, Some(http)) => {
                let mut multi = MultiIncludeLoaderAsync::new();
                if let Some(fs) = fs {
                    multi = multi.with_starts_with("file://", Box::new(fs));
                }
                Box::new(
                    multi
                        .with_starts_with("http://", Box::new(http.clone()))
                        .with_starts_with("https://", Box::new(http))
                        .with_any(Box::<NoopIncludeLoader>::default()),
                )
            }
        };
        Ok(loader)
    }
}

fn parse_origin_set(
    prefix: &str,
    env_name: &str,
    raw: Option<String>,
) -> anyhow::Result<Option<HashSet<String>>> {
    let s = raw.filter(|s| !s.is_empty());
    let Some(s) = s else {
        return Ok(None);
    };
    let mut out = HashSet::new();
    for entry in s.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let url = url::Url::parse(entry).with_context(|| {
            format!(
                "{prefix}_{env_name} entry '{entry}' must be a full origin like 'https://example.com'"
            )
        })?;
        let origin = url.origin();
        if !origin.is_tuple() {
            anyhow::bail!(
                "{prefix}_{env_name} entry '{entry}' has no tuple origin; expected 'https://host[:port]'"
            );
        }
        out.insert(origin.ascii_serialization());
    }
    Ok(Some(out))
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};
    use std::env::VarError;
    use std::time::Duration;

    use mrml::prelude::parser::loader::AsyncIncludeLoader;
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::{GuardedHttpLoader, IncludeLoaderConfig};

    fn make_lookup(
        vars: HashMap<&'static str, &'static str>,
    ) -> impl Fn(&str) -> Result<String, VarError> {
        move |key: &str| {
            vars.get(key)
                .map(ToString::to_string)
                .ok_or(VarError::NotPresent)
        }
    }

    #[test]
    fn from_env_default_returns_neither_backend() {
        let config = IncludeLoaderConfig::from_lookup(
            "CATAPULTE_INCLUDE_LOADER",
            make_lookup(HashMap::new()),
        )
        .unwrap();
        assert!(config.fs_root.is_none());
        assert!(config.http_allow.is_none());
    }

    #[test]
    fn from_env_fs_root_only_sets_fs() {
        let dir = tempfile::tempdir().unwrap();
        let raw_path = dir.path().to_str().unwrap().to_owned();
        let canonical = dir.path().canonicalize().unwrap();

        let vars = HashMap::from([(
            "CATAPULTE_INCLUDE_LOADER_FS_ROOT",
            // We need a &'static str key but the value can be dynamic via the closure.
            // Use an owned-closure variant below instead.
            "",
        )]);
        // Use a custom lookup that returns our runtime path.
        let lookup = move |key: &str| -> Result<String, VarError> {
            if key == "CATAPULTE_INCLUDE_LOADER_FS_ROOT" {
                Ok(raw_path.clone())
            } else {
                Err(VarError::NotPresent)
            }
        };

        let config = IncludeLoaderConfig::from_lookup("CATAPULTE_INCLUDE_LOADER", lookup).unwrap();
        assert_eq!(config.fs_root.as_deref().unwrap(), canonical.as_path());
        assert!(config.http_allow.is_none());
        drop(vars); // silence unused warning
    }

    #[test]
    fn from_env_fs_root_nonexistent_path_errors() {
        let lookup = |key: &str| -> Result<String, VarError> {
            if key == "CATAPULTE_INCLUDE_LOADER_FS_ROOT" {
                Ok("/this/path/does/not/exist/at/all".to_owned())
            } else {
                Err(VarError::NotPresent)
            }
        };
        let result = IncludeLoaderConfig::from_lookup("CATAPULTE_INCLUDE_LOADER", lookup);
        assert!(result.is_err());
        let msg = format!("{:#}", result.err().unwrap());
        assert!(
            msg.contains("CATAPULTE_INCLUDE_LOADER_FS_ROOT"),
            "error message should mention the env var name, got: {msg}"
        );
    }

    #[test]
    fn from_env_http_allow_only_sets_allow() {
        let vars = HashMap::from([(
            "CATAPULTE_INCLUDE_LOADER_HTTP_ALLOW",
            "https://a.example.com,https://b.example.com",
        )]);
        let config =
            IncludeLoaderConfig::from_lookup("CATAPULTE_INCLUDE_LOADER", make_lookup(vars))
                .unwrap();
        assert!(config.fs_root.is_none());
        let expected: std::collections::HashSet<String> =
            ["https://a.example.com", "https://b.example.com"]
                .iter()
                .map(ToString::to_string)
                .collect();
        assert_eq!(config.http_allow.unwrap(), expected);
    }

    #[test]
    fn from_env_http_deny_set_errors_naming_allow() {
        let vars = HashMap::from([(
            "CATAPULTE_INCLUDE_LOADER_HTTP_DENY",
            "https://b.example.com",
        )]);
        let result =
            IncludeLoaderConfig::from_lookup("CATAPULTE_INCLUDE_LOADER", make_lookup(vars));
        let msg = format!("{:#}", result.err().unwrap());
        assert!(
            msg.contains("CATAPULTE_INCLUDE_LOADER_HTTP_ALLOW"),
            "got: {msg}"
        );
    }

    #[test]
    fn from_env_http_limits_default_and_parsed() {
        let config = IncludeLoaderConfig::from_lookup(
            "CATAPULTE_INCLUDE_LOADER",
            make_lookup(HashMap::new()),
        )
        .unwrap();
        assert_eq!(config.http_timeout, Duration::from_secs(30));
        assert_eq!(config.http_max_bytes, 1_048_576);

        let vars = HashMap::from([
            ("CATAPULTE_INCLUDE_LOADER_HTTP_TIMEOUT_MS", "1500"),
            ("CATAPULTE_INCLUDE_LOADER_HTTP_MAX_BYTES", "2048"),
        ]);
        let config =
            IncludeLoaderConfig::from_lookup("CATAPULTE_INCLUDE_LOADER", make_lookup(vars))
                .unwrap();
        assert_eq!(config.http_timeout, Duration::from_millis(1500));
        assert_eq!(config.http_max_bytes, 2048);
    }

    #[test]
    fn from_env_http_limits_invalid_rejected() {
        for (key, value) in [
            ("CATAPULTE_INCLUDE_LOADER_HTTP_TIMEOUT_MS", "abc"),
            ("CATAPULTE_INCLUDE_LOADER_HTTP_TIMEOUT_MS", "0"),
            ("CATAPULTE_INCLUDE_LOADER_HTTP_MAX_BYTES", "-1"),
            ("CATAPULTE_INCLUDE_LOADER_HTTP_MAX_BYTES", "0"),
        ] {
            let vars = HashMap::from([(key, value)]);
            let result =
                IncludeLoaderConfig::from_lookup("CATAPULTE_INCLUDE_LOADER", make_lookup(vars));
            assert!(result.is_err(), "{key}={value} should be rejected");
        }
    }

    #[test]
    fn from_env_http_limit_not_unicode_rejected() {
        let lookup = |key: &str| {
            if key == "CATAPULTE_INCLUDE_LOADER_HTTP_MAX_BYTES" {
                Err(VarError::NotUnicode(std::ffi::OsString::from("x")))
            } else {
                Err(VarError::NotPresent)
            }
        };
        assert!(IncludeLoaderConfig::from_lookup("CATAPULTE_INCLUDE_LOADER", lookup).is_err());
    }

    #[tokio::test]
    async fn no_allow_list_keeps_http_includes_noop() {
        let loader = IncludeLoaderConfig::default().build().unwrap();
        assert!(loader.async_resolve("http://127.0.0.1:1/x").await.is_err());
    }

    #[test]
    fn from_env_http_allow_bare_hostname_errors() {
        let vars = HashMap::from([("CATAPULTE_INCLUDE_LOADER_HTTP_ALLOW", "example.com")]);
        let result =
            IncludeLoaderConfig::from_lookup("CATAPULTE_INCLUDE_LOADER", make_lookup(vars));
        assert!(result.is_err());
        let msg = format!("{:#}", result.err().unwrap());
        assert!(
            msg.contains("example.com"),
            "error message should mention the offending entry, got: {msg}"
        );
    }

    #[test]
    fn from_env_http_allow_unparseable_errors() {
        let vars = HashMap::from([("CATAPULTE_INCLUDE_LOADER_HTTP_ALLOW", "not a url")]);
        let result =
            IncludeLoaderConfig::from_lookup("CATAPULTE_INCLUDE_LOADER", make_lookup(vars));
        assert!(result.is_err());
        let msg = format!("{:#}", result.err().unwrap());
        assert!(
            msg.contains("not a url"),
            "error message should mention the offending entry, got: {msg}"
        );
    }

    const TIMEOUT: Duration = Duration::from_secs(30);

    fn loader(allowed: &[String], timeout: Duration, max_bytes: u64) -> GuardedHttpLoader {
        let set: HashSet<String> = allowed.iter().cloned().collect();
        GuardedHttpLoader::new(set, timeout, max_bytes).unwrap()
    }

    async fn counting_server() -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("<mj-text>x</mj-text>"))
            .expect(0)
            .mount(&server)
            .await;
        server
    }

    #[tokio::test]
    async fn allowed_origin_fetches_body() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("<mj-text>ok</mj-text>"))
            .expect(1)
            .mount(&server)
            .await;
        let l = loader(&[server.uri()], TIMEOUT, 1024);

        let body = l.async_resolve(&format!("{}/a.mjml", server.uri())).await;

        assert_eq!(body.unwrap(), "<mj-text>ok</mj-text>");
    }

    #[tokio::test]
    async fn redirect_is_not_followed() {
        let target = counting_server().await;
        let origin = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(302).insert_header("location", format!("{}/x", target.uri())),
            )
            .expect(1)
            .mount(&origin)
            .await;
        let l = loader(&[origin.uri()], TIMEOUT, 1024);

        assert!(
            l.async_resolve(&format!("{}/a", origin.uri()))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn oversize_include_is_rejected() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("x".repeat(64)))
            .mount(&server)
            .await;
        let l = loader(&[server.uri()], TIMEOUT, 16);

        assert!(
            l.async_resolve(&format!("{}/a", server.uri()))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn disallowed_origin_sends_no_request() {
        let target = counting_server().await;
        let l = loader(&["https://allowed.test".to_owned()], TIMEOUT, 1024);

        assert!(
            l.async_resolve(&format!("{}/a", target.uri()))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn same_host_different_port_is_rejected() {
        let allowed = counting_server().await;
        let other = counting_server().await;
        let l = loader(&[allowed.uri()], TIMEOUT, 1024);

        assert!(
            l.async_resolve(&format!("{}/a", other.uri()))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn deceptive_userinfo_is_rejected() {
        let target = counting_server().await;
        let l = loader(&["http://allowed.test".to_owned()], TIMEOUT, 1024);
        let path = format!("http://allowed.test@{}/x", target.address());

        assert!(l.async_resolve(&path).await.is_err());
    }

    /// Serves one HTTP/1.1 response; each step is written after its delay.
    async fn serve_raw(steps: Vec<(Duration, Vec<u8>)>) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = [0_u8; 4096];
            let _ = socket.read(&mut buf).await;
            for (delay, bytes) in steps {
                tokio::time::sleep(delay).await;
                if socket.write_all(&bytes).await.is_err() {
                    return;
                }
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        });
        origin
    }

    const CHUNKED_HEAD: &[u8] = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n";

    #[tokio::test]
    async fn chunked_body_over_cap_is_rejected() {
        let mut steps = vec![(Duration::ZERO, CHUNKED_HEAD.to_vec())];
        for _ in 0..3 {
            steps.push((Duration::ZERO, b"8\r\nxxxxxxxx\r\n".to_vec()));
        }
        steps.push((Duration::ZERO, b"0\r\n\r\n".to_vec()));
        let origin = serve_raw(steps).await;
        let l = loader(std::slice::from_ref(&origin), TIMEOUT, 16);

        assert!(l.async_resolve(&format!("{origin}/a")).await.is_err());
    }

    #[tokio::test]
    async fn stalled_body_times_out() {
        let steps = vec![
            (Duration::ZERO, CHUNKED_HEAD.to_vec()),
            (Duration::from_millis(600), b"1\r\nx\r\n0\r\n\r\n".to_vec()),
        ];
        let origin = serve_raw(steps).await;
        let l = loader(
            std::slice::from_ref(&origin),
            Duration::from_millis(100),
            1024,
        );

        assert!(l.async_resolve(&format!("{origin}/a")).await.is_err());
    }

    #[tokio::test]
    async fn body_exactly_at_cap_succeeds() {
        let steps = vec![(
            Duration::ZERO,
            b"HTTP/1.1 200 OK\r\nContent-Length: 16\r\n\r\nxxxxxxxxxxxxxxxx".to_vec(),
        )];
        let origin = serve_raw(steps).await;
        let l = loader(std::slice::from_ref(&origin), TIMEOUT, 16);

        let body = l.async_resolve(&format!("{origin}/a")).await.unwrap();

        assert_eq!(body.len(), 16);
    }

    #[tokio::test]
    async fn invalid_utf8_body_is_rejected() {
        let steps = vec![(
            Duration::ZERO,
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n\xff\xfe".to_vec(),
        )];
        let origin = serve_raw(steps).await;
        let l = loader(std::slice::from_ref(&origin), TIMEOUT, 1024);

        assert!(l.async_resolve(&format!("{origin}/a")).await.is_err());
    }

    #[test]
    fn from_env_http_deny_not_unicode_errors() {
        let lookup = |key: &str| {
            if key == "CATAPULTE_INCLUDE_LOADER_HTTP_DENY" {
                Err(VarError::NotUnicode(std::ffi::OsString::from("x")))
            } else {
                Err(VarError::NotPresent)
            }
        };
        let result = IncludeLoaderConfig::from_lookup("CATAPULTE_INCLUDE_LOADER", lookup);
        let msg = format!("{:#}", result.err().unwrap());
        assert!(
            msg.contains("CATAPULTE_INCLUDE_LOADER_HTTP_ALLOW"),
            "got: {msg}"
        );
    }

    #[test]
    fn origin_comparison_table() {
        // (allowed entry, requested url, expected to match)
        let cases = [
            ("http://127.0.0.1:8080", "HTTP://127.0.0.1:8080/x", true),
            ("http://EXAMPLE.test", "http://example.test/x", true),
            ("http://example.test", "http://example.test:80/x", true),
            ("http://example.test:80", "http://example.test/x", true),
            ("https://example.test", "https://example.test:443/x", true),
            ("http://example.test", "http://example.test:8080/x", false),
            ("http://example.test", "https://example.test/x", false),
            ("http://example.test", "http://example.test./x", false),
            ("http://example.test.", "http://example.test/x", false),
            ("http://[::1]:8080", "http://[::1]:8080/x", true),
            ("http://[::1]:8080", "http://127.0.0.1:8080/x", false),
            ("http://bücher.test", "http://xn--bcher-kva.test/x", true),
            ("http://xn--bcher-kva.test", "http://bücher.test/x", true),
            (
                "http://allowed.test",
                "http://allowed.test@127.0.0.1:8080/x",
                false,
            ),
        ];
        for (allowed, requested, expected) in cases {
            let set = super::parse_origin_set("P", "X", Some(allowed.to_owned()))
                .unwrap()
                .unwrap();
            let origin = url::Url::parse(requested)
                .unwrap()
                .origin()
                .ascii_serialization();
            assert_eq!(
                set.contains(&origin),
                expected,
                "allowed={allowed} requested={requested}"
            );
        }
    }

    #[tokio::test]
    async fn uppercase_scheme_request_to_allowed_origin_succeeds() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
            .expect(1)
            .mount(&server)
            .await;
        let l = loader(&[server.uri()], TIMEOUT, 1024);
        let path = format!("{}/a", server.uri().replacen("http", "HTTP", 1));

        assert_eq!(l.async_resolve(&path).await.unwrap(), "ok");
    }

    #[tokio::test]
    async fn ipv6_literal_origin_is_matched() {
        let Ok(listener) = std::net::TcpListener::bind("[::1]:0") else {
            return;
        };
        let server = MockServer::builder().listener(listener).start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("v6"))
            .expect(1)
            .mount(&server)
            .await;
        let l = loader(&[server.uri()], TIMEOUT, 1024);

        assert_eq!(
            l.async_resolve(&format!("{}/a", server.uri()))
                .await
                .unwrap(),
            "v6"
        );
    }

    #[tokio::test]
    async fn not_found_and_server_error_are_rejected() {
        for status in [404, 500] {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .respond_with(ResponseTemplate::new(status).set_body_string("body"))
                .expect(1)
                .mount(&server)
                .await;
            let l = loader(&[server.uri()], TIMEOUT, 1024);

            let result = l.async_resolve(&format!("{}/a", server.uri())).await;

            assert!(result.is_err(), "status {status} should be rejected");
        }
    }
}
