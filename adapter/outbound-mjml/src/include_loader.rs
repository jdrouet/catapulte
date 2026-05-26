use std::collections::HashSet;
use std::env::VarError;
use std::path::PathBuf;

use anyhow::Context;
use mrml::prelude::parser::http_loader::{HttpIncludeLoader, UreqFetcher};
use mrml::prelude::parser::loader::{IncludeLoader, IncludeLoaderError};
use mrml::prelude::parser::local_loader::LocalIncludeLoader;
use mrml::prelude::parser::noop_loader::NoopIncludeLoader;

#[derive(Debug)]
enum HttpOriginConfig {
    Allow(HashSet<String>),
    Deny(HashSet<String>),
}

#[derive(Default)]
pub struct IncludeLoaderConfig {
    pub fs_root: Option<PathBuf>,
    http_origin: Option<HttpOriginConfig>,
}

impl IncludeLoaderConfig {
    /// Reads:
    /// - `<prefix>_FS_ROOT`     (path, optional)
    /// - `<prefix>_HTTP_ALLOW`  (comma-separated origins, optional)
    /// - `<prefix>_HTTP_DENY`   (comma-separated origins, optional)
    ///
    /// `HTTP_ALLOW` and `HTTP_DENY` are mutually exclusive.
    ///
    /// # Errors
    ///
    /// Returns an error if both `HTTP_ALLOW` and `HTTP_DENY` are set.
    pub fn from_env(prefix: &str) -> anyhow::Result<Self> {
        Self::from_lookup(prefix, |key| std::env::var(key))
    }

    fn from_lookup<F>(prefix: &str, lookup: F) -> anyhow::Result<Self>
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

        let allow = parse_origin_set(
            prefix,
            "HTTP_ALLOW",
            lookup(&format!("{prefix}_HTTP_ALLOW")).ok(),
        )?;
        let deny = parse_origin_set(
            prefix,
            "HTTP_DENY",
            lookup(&format!("{prefix}_HTTP_DENY")).ok(),
        )?;

        let http_origin = match (allow, deny) {
            (Some(_), Some(_)) => {
                anyhow::bail!("{prefix}_HTTP_ALLOW and {prefix}_HTTP_DENY are mutually exclusive")
            }
            (Some(a), None) => Some(HttpOriginConfig::Allow(a)),
            (None, Some(d)) => Some(HttpOriginConfig::Deny(d)),
            (None, None) => None,
        };

        Ok(Self {
            fs_root,
            http_origin,
        })
    }

    pub fn build(self) -> Box<dyn IncludeLoader + Send + Sync> {
        let fs: Option<LocalIncludeLoader> = self.fs_root.map(LocalIncludeLoader::new);
        let http_origin = self.http_origin;

        match (fs, http_origin) {
            (None, None) => Box::new(NoopIncludeLoader),
            (Some(fs), None) => Box::new(fs),
            (None, Some(origin)) => Box::new(build_http_loader(origin)),
            (Some(fs), Some(origin)) => {
                // Two separate loaders because HttpIncludeLoader is not Clone.
                let (loader_http, loader_https) = build_http_loader_pair(origin);
                Box::new(PrefixDispatchLoader {
                    fs: Some(fs),
                    http: Some(loader_http),
                    https: Some(loader_https),
                })
            }
        }
    }
}

/// Dispatches includes by URL scheme: `file://` → `LocalIncludeLoader`,
/// `http://` / `https://` → `HttpIncludeLoader`, anything else → `NotFound`.
///
/// Used only when both the filesystem and HTTP backends are configured.
#[derive(Debug)]
struct PrefixDispatchLoader {
    fs: Option<LocalIncludeLoader>,
    http: Option<HttpIncludeLoader<UreqFetcher>>,
    https: Option<HttpIncludeLoader<UreqFetcher>>,
}

impl IncludeLoader for PrefixDispatchLoader {
    fn resolve(&self, path: &str) -> Result<String, IncludeLoaderError> {
        if path.starts_with("file://") {
            self.fs.as_ref().map_or_else(
                || Err(IncludeLoaderError::not_found(path)),
                |l| l.resolve(path),
            )
        } else if path.starts_with("http://") {
            self.http.as_ref().map_or_else(
                || Err(IncludeLoaderError::not_found(path)),
                |l| l.resolve(path),
            )
        } else if path.starts_with("https://") {
            self.https.as_ref().map_or_else(
                || Err(IncludeLoaderError::not_found(path)),
                |l| l.resolve(path),
            )
        } else {
            Err(IncludeLoaderError::not_found(path))
        }
    }
}

fn build_http_loader(origin: HttpOriginConfig) -> HttpIncludeLoader<UreqFetcher> {
    match origin {
        HttpOriginConfig::Allow(set) => HttpIncludeLoader::<UreqFetcher>::new_allow(set),
        HttpOriginConfig::Deny(set) => HttpIncludeLoader::<UreqFetcher>::new_deny(set),
    }
}

fn build_http_loader_pair(
    origin: HttpOriginConfig,
) -> (
    HttpIncludeLoader<UreqFetcher>,
    HttpIncludeLoader<UreqFetcher>,
) {
    match origin {
        HttpOriginConfig::Allow(set) => (
            HttpIncludeLoader::<UreqFetcher>::new_allow(set.clone()),
            HttpIncludeLoader::<UreqFetcher>::new_allow(set),
        ),
        HttpOriginConfig::Deny(set) => (
            HttpIncludeLoader::<UreqFetcher>::new_deny(set.clone()),
            HttpIncludeLoader::<UreqFetcher>::new_deny(set),
        ),
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
    use std::collections::HashMap;
    use std::env::VarError;

    use super::IncludeLoaderConfig;

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
        assert!(config.http_origin.is_none());
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
        assert!(config.http_origin.is_none());
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
        use super::HttpOriginConfig;

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
        match config.http_origin.unwrap() {
            HttpOriginConfig::Allow(set) => assert_eq!(set, expected),
            HttpOriginConfig::Deny(_) => panic!("expected Allow"),
        }
    }

    #[test]
    fn from_env_http_deny_only_sets_deny() {
        use super::HttpOriginConfig;

        let vars = HashMap::from([(
            "CATAPULTE_INCLUDE_LOADER_HTTP_DENY",
            "https://x.example.com",
        )]);
        let config =
            IncludeLoaderConfig::from_lookup("CATAPULTE_INCLUDE_LOADER", make_lookup(vars))
                .unwrap();
        assert!(config.fs_root.is_none());
        let expected: std::collections::HashSet<String> = ["https://x.example.com"]
            .iter()
            .map(ToString::to_string)
            .collect();
        match config.http_origin.unwrap() {
            HttpOriginConfig::Deny(set) => assert_eq!(set, expected),
            HttpOriginConfig::Allow(_) => panic!("expected Deny"),
        }
    }

    #[test]
    fn from_env_both_allow_and_deny_errors() {
        let vars = HashMap::from([
            (
                "CATAPULTE_INCLUDE_LOADER_HTTP_ALLOW",
                "https://a.example.com",
            ),
            (
                "CATAPULTE_INCLUDE_LOADER_HTTP_DENY",
                "https://b.example.com",
            ),
        ]);
        let result =
            IncludeLoaderConfig::from_lookup("CATAPULTE_INCLUDE_LOADER", make_lookup(vars));
        assert!(result.is_err());
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
}
