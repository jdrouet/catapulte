use std::env::VarError;

use anyhow::Context;
use catapulte_domain::entity::sender::{QuotaRange, SenderName, SenderQuota};

use crate::transport::{SmtpConfig, SmtpTransport};

pub struct SmtpTransportEntry {
    pub name: SenderName,
    pub priority: u8,
    pub quota: Option<SenderQuota>,
    pub transport: SmtpTransport,
}

struct SingleSenderConfig {
    name: SenderName,
    smtp: SmtpConfig,
    priority: u8,
    quota: Option<SenderQuota>,
}

/// Configuration for a collection of SMTP senders.
///
/// Environment variables:
/// - `CATAPULTE_SENDERS`: comma-separated list of sender names
/// - For each `NAME`:
///   - `CATAPULTE_SENDER_{NAME}_HOST` (required)
///   - `CATAPULTE_SENDER_{NAME}_PORT` (optional, default 587)
///   - `CATAPULTE_SENDER_{NAME}_USERNAME` (optional)
///   - `CATAPULTE_SENDER_{NAME}_PASSWORD` (optional)
///   - `CATAPULTE_SENDER_{NAME}_TLS` (optional, default "starttls")
///   - `CATAPULTE_SENDER_{NAME}_PRIORITY` (optional, default 100)
///   - `CATAPULTE_SENDER_{NAME}_QUOTA_COUNT` (optional)
///   - `CATAPULTE_SENDER_{NAME}_QUOTA_RANGE` (optional: "hourly", "daily",
///     "weekly", "monthly")
pub struct MultiSenderConfig {
    senders: Vec<SingleSenderConfig>,
}

impl MultiSenderConfig {
    /// Creates a config with a single unnamed sender using the given `SmtpConfig`.
    /// Useful for tests and simple single-server setups.
    #[must_use]
    pub fn single(name: impl Into<String>, smtp: SmtpConfig) -> Self {
        Self {
            senders: vec![SingleSenderConfig {
                name: SenderName::new(name),
                smtp,
                priority: 100,
                quota: None,
            }],
        }
    }

    /// Creates an empty config (use with `with_sender` to add entries).
    #[must_use]
    pub fn empty() -> Self {
        Self { senders: vec![] }
    }

    /// Adds a named sender entry to this config (sorted by priority at build time).
    /// Useful for constructing multi-sender configs in tests.
    #[must_use]
    pub fn with_sender(
        mut self,
        name: impl Into<String>,
        smtp: SmtpConfig,
        priority: u8,
        quota: Option<SenderQuota>,
    ) -> Self {
        self.senders.push(SingleSenderConfig {
            name: SenderName::new(name),
            smtp,
            priority,
            quota,
        });
        self.senders.sort_by_key(|s| s.priority);
        self
    }

    /// Reads all sender configuration from the real environment.
    ///
    /// # Errors
    ///
    /// Returns an error if a required variable is missing or has an invalid
    /// value.
    pub fn from_env() -> anyhow::Result<Self> {
        Self::from_lookup(|key| std::env::var(key))
    }

    fn from_lookup<F>(lookup: F) -> anyhow::Result<Self>
    where
        F: Fn(&str) -> Result<String, VarError>,
    {
        let names_raw = lookup("CATAPULTE_SENDERS").context("missing env var CATAPULTE_SENDERS")?;

        let mut senders: Vec<SingleSenderConfig> = names_raw
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|raw_name| {
                let upper = raw_name.to_uppercase();
                let prefix = format!("CATAPULTE_SENDER_{upper}");
                let smtp = SmtpConfig::from_lookup(&prefix, &lookup)?;
                let priority = parse_priority(
                    lookup(&format!("{prefix}_PRIORITY")).ok(),
                    &format!("{prefix}_PRIORITY"),
                )?;
                let quota = parse_quota(&prefix, &lookup)?;
                Ok(SingleSenderConfig {
                    name: SenderName::new(raw_name),
                    smtp,
                    priority,
                    quota,
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;

        senders.sort_by_key(|s| s.priority);

        Ok(Self { senders })
    }

    /// # Errors
    ///
    /// Returns an error if any SMTP transport cannot be constructed.
    pub fn build(self) -> anyhow::Result<Vec<SmtpTransportEntry>> {
        self.senders
            .into_iter()
            .map(|cfg| {
                let transport = cfg.smtp.build()?;
                Ok(SmtpTransportEntry {
                    name: cfg.name,
                    priority: cfg.priority,
                    quota: cfg.quota,
                    transport,
                })
            })
            .collect()
    }
}

fn parse_priority(raw: Option<String>, key: &str) -> anyhow::Result<u8> {
    match raw {
        None => Ok(100),
        Some(val) => val
            .parse::<u8>()
            .with_context(|| format!("invalid value for env var {key}")),
    }
}

fn parse_quota_range(val: &str, key: &str) -> anyhow::Result<QuotaRange> {
    match val {
        "hourly" => Ok(QuotaRange::Hourly),
        "daily" => Ok(QuotaRange::Daily),
        "weekly" => Ok(QuotaRange::Weekly),
        "monthly" => Ok(QuotaRange::Monthly),
        other => anyhow::bail!("unknown value for env var {key}: {other}"),
    }
}

fn parse_quota<F>(prefix: &str, lookup: &F) -> anyhow::Result<Option<SenderQuota>>
where
    F: Fn(&str) -> Result<String, VarError>,
{
    let count_key = format!("{prefix}_QUOTA_COUNT");
    let range_key = format!("{prefix}_QUOTA_RANGE");

    let count_raw = lookup(&count_key).ok();
    let range_raw = lookup(&range_key).ok();

    match (count_raw, range_raw) {
        (None, None) => Ok(None),
        (Some(count_str), Some(range_str)) => {
            let count = count_str
                .parse::<u64>()
                .with_context(|| format!("invalid value for env var {count_key}"))?;
            let range = parse_quota_range(&range_str, &range_key)?;
            Ok(Some(SenderQuota { count, range }))
        }
        (Some(_), None) => {
            anyhow::bail!("env var {count_key} is set but {range_key} is missing")
        }
        (None, Some(_)) => {
            anyhow::bail!("env var {range_key} is set but {count_key} is missing")
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::env::VarError;

    use catapulte_domain::entity::sender::QuotaRange;

    use super::MultiSenderConfig;

    fn make_lookup(
        vars: HashMap<&'static str, &'static str>,
    ) -> impl Fn(&str) -> Result<String, VarError> {
        move |key: &str| {
            vars.get(key)
                .map(|v| (*v).to_owned())
                .ok_or(VarError::NotPresent)
        }
    }

    #[test]
    fn single_sender_parsed_correctly() {
        let mut vars = HashMap::new();
        vars.insert("CATAPULTE_SENDERS", "primary");
        vars.insert("CATAPULTE_SENDER_PRIMARY_HOST", "smtp.example.com");
        let config = MultiSenderConfig::from_lookup(make_lookup(vars)).unwrap();
        assert_eq!(config.senders.len(), 1);
        assert_eq!(config.senders[0].name.as_str(), "primary");
        assert_eq!(config.senders[0].priority, 100);
        assert!(config.senders[0].quota.is_none());
    }

    #[test]
    fn multiple_senders_sorted_by_priority() {
        let mut vars = HashMap::new();
        vars.insert("CATAPULTE_SENDERS", "slow,fast");
        vars.insert("CATAPULTE_SENDER_SLOW_HOST", "slow.example.com");
        vars.insert("CATAPULTE_SENDER_SLOW_PRIORITY", "200");
        vars.insert("CATAPULTE_SENDER_FAST_HOST", "fast.example.com");
        vars.insert("CATAPULTE_SENDER_FAST_PRIORITY", "10");
        let config = MultiSenderConfig::from_lookup(make_lookup(vars)).unwrap();
        assert_eq!(config.senders.len(), 2);
        assert_eq!(config.senders[0].name.as_str(), "fast");
        assert_eq!(config.senders[1].name.as_str(), "slow");
    }

    #[test]
    fn missing_host_returns_error() {
        let mut vars = HashMap::new();
        vars.insert("CATAPULTE_SENDERS", "primary");
        // CATAPULTE_SENDER_PRIMARY_HOST intentionally absent.
        let result = MultiSenderConfig::from_lookup(make_lookup(vars));
        assert!(result.is_err());
    }

    #[test]
    fn invalid_priority_returns_error() {
        let mut vars = HashMap::new();
        vars.insert("CATAPULTE_SENDERS", "primary");
        vars.insert("CATAPULTE_SENDER_PRIMARY_HOST", "smtp.example.com");
        vars.insert("CATAPULTE_SENDER_PRIMARY_PRIORITY", "not-a-number");
        let result = MultiSenderConfig::from_lookup(make_lookup(vars));
        assert!(result.is_err());
    }

    #[test]
    fn quota_parsed_correctly() {
        let mut vars = HashMap::new();
        vars.insert("CATAPULTE_SENDERS", "primary");
        vars.insert("CATAPULTE_SENDER_PRIMARY_HOST", "smtp.example.com");
        vars.insert("CATAPULTE_SENDER_PRIMARY_QUOTA_COUNT", "500");
        vars.insert("CATAPULTE_SENDER_PRIMARY_QUOTA_RANGE", "daily");
        let config = MultiSenderConfig::from_lookup(make_lookup(vars)).unwrap();
        let quota = config.senders[0].quota.as_ref().unwrap();
        assert_eq!(quota.count, 500);
        assert_eq!(quota.range, QuotaRange::Daily);
    }
}
