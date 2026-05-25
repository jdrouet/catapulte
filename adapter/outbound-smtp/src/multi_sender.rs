use std::env::VarError;
use std::pin::Pin;

use anyhow::Context;
use catapulte_domain::entity::sender::{QuotaRange, SenderName, SenderQuota};
use catapulte_domain::port::email_sender::{EmailSender, OutboundEmail, SendError};

use crate::sender::{SmtpConfig, SmtpSender};

/// Internal abstraction that mirrors `EmailSender` but takes `email` by
/// reference.  This lets `MultiSmtpSender` iterate over senders without
/// consuming `OutboundEmail` on the first attempt.
pub(crate) trait SendRef: Send + Sync {
    fn send_ref<'a>(
        &'a self,
        email: &'a OutboundEmail,
    ) -> Pin<Box<dyn Future<Output = Result<SenderName, SendError>> + Send + 'a>>;
}

impl SendRef for SmtpSender {
    fn send_ref<'a>(
        &'a self,
        email: &'a OutboundEmail,
    ) -> Pin<Box<dyn Future<Output = Result<SenderName, SendError>> + Send + 'a>> {
        Box::pin(async move {
            self.send_inner(email)
                .await
                .map_err(|source| SendError::Send {
                    sender_name: self.name().clone(),
                    source,
                })?;
            Ok(self.name().clone())
        })
    }
}

struct SenderEntry<S> {
    sender: S,
}

/// Tries each contained sender in priority order (ascending); falls through
/// to the next on `SendError`.  Returns the last error if all senders fail.
pub struct MultiSmtpSender<S = SmtpSender> {
    /// Sorted by `priority` ascending at construction time; index 0 is tried
    /// first.
    senders: Vec<SenderEntry<S>>,
}

impl<S: SendRef> EmailSender for MultiSmtpSender<S> {
    async fn send(&self, email: OutboundEmail) -> Result<SenderName, SendError> {
        let mut last_err: Option<SendError> = None;
        for entry in &self.senders {
            match entry.sender.send_ref(&email).await {
                Ok(name) => return Ok(name),
                Err(err) => {
                    last_err = Some(err);
                }
            }
        }
        Err(last_err.expect("senders list must not be empty"))
    }
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

    /// Builds a `MultiSmtpSender` from the configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if any SMTP transport cannot be constructed.
    pub fn build(self) -> anyhow::Result<MultiSmtpSender> {
        let entries = self
            .senders
            .into_iter()
            .map(|cfg| {
                let sender = cfg.smtp.build_named(cfg.name)?;
                Ok(SenderEntry { sender })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok(MultiSmtpSender { senders: entries })
    }

    /// Returns the list of sender names and their optional quotas without
    /// building any SMTP transport.  Useful for registering senders with the
    /// quota subsystem at startup.
    #[must_use]
    pub fn sender_caps(&self) -> Vec<(SenderName, Option<SenderQuota>)> {
        self.senders
            .iter()
            .map(|cfg| (cfg.name.clone(), cfg.quota.clone()))
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
    use std::future::Future;
    use std::pin::Pin;

    use catapulte_domain::entity::body::{Plain, RenderedBody};
    use catapulte_domain::entity::email::RecipientKind;
    use catapulte_domain::entity::sender::{QuotaRange, SenderName};
    use catapulte_domain::port::email_sender::{EmailSender, OutboundEmail, SendError};

    use super::{MultiSenderConfig, MultiSmtpSender, SendRef, SenderEntry};

    fn make_lookup(
        vars: HashMap<&'static str, &'static str>,
    ) -> impl Fn(&str) -> Result<String, VarError> {
        move |key: &str| {
            vars.get(key)
                .map(|v| (*v).to_owned())
                .ok_or(VarError::NotPresent)
        }
    }

    fn make_email() -> OutboundEmail {
        let plain = Plain::try_new(Some("hello".to_string()), None).unwrap();
        OutboundEmail {
            sender: "from@example.com".to_string(),
            subject: None,
            recipients: vec![(RecipientKind::To, "to@example.com".to_string())],
            body: RenderedBody::new(plain),
        }
    }

    struct FailSender {
        name: SenderName,
    }

    impl SendRef for FailSender {
        fn send_ref<'a>(
            &'a self,
            _email: &'a OutboundEmail,
        ) -> Pin<Box<dyn Future<Output = Result<SenderName, SendError>> + Send + 'a>> {
            let err = SendError::Send {
                sender_name: self.name.clone(),
                source: anyhow::anyhow!("simulated failure"),
            };
            Box::pin(async move { Err(err) })
        }
    }

    struct OkSender {
        name: SenderName,
    }

    impl SendRef for OkSender {
        fn send_ref<'a>(
            &'a self,
            _email: &'a OutboundEmail,
        ) -> Pin<Box<dyn Future<Output = Result<SenderName, SendError>> + Send + 'a>> {
            let name = self.name.clone();
            Box::pin(async move { Ok(name) })
        }
    }

    enum FakeSender {
        Fail(FailSender),
        Ok(OkSender),
    }

    impl SendRef for FakeSender {
        fn send_ref<'a>(
            &'a self,
            email: &'a OutboundEmail,
        ) -> Pin<Box<dyn Future<Output = Result<SenderName, SendError>> + Send + 'a>> {
            match self {
                Self::Fail(s) => s.send_ref(email),
                Self::Ok(s) => s.send_ref(email),
            }
        }
    }

    impl EmailSender for FakeSender {
        fn send(
            &self,
            email: OutboundEmail,
        ) -> impl Future<Output = Result<SenderName, SendError>> + Send {
            async move { self.send_ref(&email).await }
        }
    }

    fn make_multi(senders: Vec<FakeSender>) -> MultiSmtpSender<FakeSender> {
        let entries = senders
            .into_iter()
            .map(|sender| SenderEntry { sender })
            .collect();
        MultiSmtpSender { senders: entries }
    }

    #[tokio::test]
    async fn first_sender_fails_second_succeeds() {
        let multi = make_multi(vec![
            FakeSender::Fail(FailSender {
                name: SenderName::new("first"),
            }),
            FakeSender::Ok(OkSender {
                name: SenderName::new("second"),
            }),
        ]);
        let result = multi.send(make_email()).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap().as_str(), "second");
    }

    #[tokio::test]
    async fn all_senders_fail_returns_last_error() {
        let multi = make_multi(vec![
            FakeSender::Fail(FailSender {
                name: SenderName::new("first"),
            }),
            FakeSender::Fail(FailSender {
                name: SenderName::new("second"),
            }),
        ]);
        let result = multi.send(make_email()).await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().sender_name().as_str(), "second");
    }

    #[tokio::test]
    async fn first_sender_succeeds_returns_immediately() {
        let multi = make_multi(vec![
            FakeSender::Ok(OkSender {
                name: SenderName::new("first"),
            }),
            FakeSender::Ok(OkSender {
                name: SenderName::new("second"),
            }),
        ]);
        let result = multi.send(make_email()).await;
        assert_eq!(result.unwrap().as_str(), "first");
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
