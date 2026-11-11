use std::collections::HashSet;
use std::env::VarError;

use anyhow::Context;
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use catapulte_domain::use_case::submit_email::{SubmitEmailInput, normalize_sender_domains};

/// Who is calling. Set only by the auth middleware as a request extension, never read from a
/// request body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApiKeyIdentity {
    /// Namespace for idempotency keys and read scoping; `""` for the legacy key and trusted mode.
    pub owner: String,
    pub admin: bool,
    /// Sender domains this key may use, intersected with the global allow-list. `Some` of an
    /// empty set denies every sender.
    pub sender_domains: Option<HashSet<String>>,
}

impl ApiKeyIdentity {
    /// Identity used when no key of any kind is configured.
    #[must_use]
    pub fn trusted() -> Self {
        Self {
            owner: String::new(),
            admin: true,
            sender_domains: None,
        }
    }

    /// Stamps the submission with this caller's namespace and sender restriction.
    #[must_use]
    pub fn scope(&self, mut input: SubmitEmailInput) -> SubmitEmailInput {
        input.owner.clone_from(&self.owner);
        input.sender_domains.clone_from(&self.sender_domains);
        input
    }

    /// Owner that list queries must be restricted to; `None` for an admin.
    #[must_use]
    pub fn read_scope(&self) -> Option<String> {
        (!self.admin).then(|| self.owner.clone())
    }
}

pub struct NamedApiKey {
    pub name: String,
    pub secret: String,
    pub admin: bool,
    pub sender_domains: Option<HashSet<String>>,
}

struct AuthKey {
    secret: String,
    identity: ApiKeyIdentity,
}

/// Every accepted bearer secret and the identity it grants.
pub struct AuthConfig {
    keys: Vec<AuthKey>,
}

/// Only an absent variable counts as unset; a value that is not valid unicode must stop
/// startup, because ignoring it would silently drop a key or a restriction.
fn optional_var<F>(lookup: &F, key: &str) -> anyhow::Result<Option<String>>
where
    F: Fn(&str) -> Result<String, VarError>,
{
    match lookup(key) {
        Ok(value) => Ok(Some(value)),
        Err(VarError::NotPresent) => Ok(None),
        Err(VarError::NotUnicode(_)) => anyhow::bail!("{key} is not valid unicode"),
    }
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

impl AuthConfig {
    /// Builds the auth config from the legacy key and the named keys; `None` when neither is
    /// configured, which means authentication is off.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid or repeated key name, an empty secret, or a secret
    /// shared by two keys.
    pub fn new(legacy: Option<String>, named: Vec<NamedApiKey>) -> anyhow::Result<Option<Self>> {
        let mut keys = Vec::new();
        if let Some(secret) = legacy {
            keys.push(AuthKey {
                secret,
                identity: ApiKeyIdentity {
                    owner: String::new(),
                    admin: true,
                    sender_domains: None,
                },
            });
        }
        let mut seen = HashSet::new();
        for key in named {
            anyhow::ensure!(
                valid_name(&key.name),
                "invalid API key name {:?}: use lowercase letters, digits and underscores",
                key.name
            );
            anyhow::ensure!(
                seen.insert(key.name.clone()),
                "API key name {:?} is listed twice",
                key.name
            );
            anyhow::ensure!(
                !key.secret.is_empty(),
                "API key {:?} has an empty secret",
                key.name
            );
            keys.push(AuthKey {
                secret: key.secret,
                identity: ApiKeyIdentity {
                    owner: key.name,
                    admin: key.admin,
                    sender_domains: key.sender_domains,
                },
            });
        }
        let mut secrets = HashSet::new();
        anyhow::ensure!(
            keys.iter().all(|k| secrets.insert(k.secret.as_str())),
            "two API keys share the same secret"
        );
        Ok((!keys.is_empty()).then_some(Self { keys }))
    }

    /// Reads the legacy key from `legacy_key_var`, then `CATAPULTE_API_KEYS` and, per name,
    /// `CATAPULTE_API_KEY_<NAME>_SECRET`, `_SENDER_DOMAINS` and `_ADMIN`. An empty legacy key
    /// or secret counts as unset.
    ///
    /// # Errors
    ///
    /// Returns an error when a variable is not valid unicode, when a named key is incomplete
    /// or invalid, or when [`AuthConfig::new`] rejects the set.
    pub fn from_lookup<F>(legacy_key_var: &str, lookup: F) -> anyhow::Result<Option<Self>>
    where
        F: Fn(&str) -> Result<String, VarError>,
    {
        let legacy = optional_var(&lookup, legacy_key_var)?
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty());
        let list = optional_var(&lookup, "CATAPULTE_API_KEYS")?.unwrap_or_default();
        let named = list
            .split(',')
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .map(|name| {
                let prefix = format!("CATAPULTE_API_KEY_{}", name.to_uppercase());
                let secret_key = format!("{prefix}_SECRET");
                let secret = optional_var(&lookup, &secret_key)?
                    .map(|v| v.trim().to_owned())
                    .filter(|v| !v.is_empty())
                    .with_context(|| format!("missing env var {secret_key}"))?;
                let domains_key = format!("{prefix}_SENDER_DOMAINS");
                let sender_domains = optional_var(&lookup, &domains_key)?
                    .map(|raw| {
                        normalize_sender_domains(
                            raw.split(',').map(str::trim).filter(|d| !d.is_empty()),
                        )
                        .with_context(|| format!("invalid {domains_key}"))
                    })
                    .transpose()?;
                let admin_key = format!("{prefix}_ADMIN");
                let admin = match optional_var(&lookup, &admin_key)?.as_deref() {
                    None | Some("false") => false,
                    Some("true") => true,
                    Some(other) => anyhow::bail!("invalid {admin_key}: {other:?}"),
                };
                Ok(NamedApiKey {
                    name: name.to_owned(),
                    secret,
                    admin,
                    sender_domains,
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        Self::new(legacy, named)
    }

    /// Compares the token to every secret, with no early exit.
    pub(crate) fn authenticate(&self, token: &str) -> Option<&ApiKeyIdentity> {
        let mut found = None;
        for key in &self.keys {
            if constant_time_eq(token.as_bytes(), key.secret.as_bytes()) {
                found = Some(&key.identity);
            }
        }
        found
    }
}

/// Compares two byte slices in constant time to avoid timing side-channels.
///
/// A length mismatch short-circuits before the byte loop, which leaks the length; this is
/// considered acceptable.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Requires `Authorization: Bearer <secret>` matching a configured key and records the
/// caller's identity for the handlers.
pub(crate) async fn require_api_key(
    State(auth): State<std::sync::Arc<AuthConfig>>,
    mut request: Request,
    next: Next,
) -> Result<axum::response::Response, StatusCode> {
    let token = request
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or(StatusCode::UNAUTHORIZED)?;
    let identity = auth
        .authenticate(token)
        .cloned()
        .ok_or(StatusCode::UNAUTHORIZED)?;
    request.extensions_mut().insert(identity);
    Ok(next.run(request).await)
}

/// Used when authentication is off: every caller is the trusted administrator.
pub(crate) async fn trust_every_caller(
    mut request: Request,
    next: Next,
) -> axum::response::Response {
    request.extensions_mut().insert(ApiKeyIdentity::trusted());
    next.run(request).await
}
