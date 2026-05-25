use std::collections::HashMap;
use std::env::VarError;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use catapulte_domain::entity::sender::{QuotaRange, SenderName, SenderQuota};
use catapulte_domain::port::email_sender::{EmailSender, OutboundEmail, SendError};
use catapulte_domain::port::sender_repository::{
    SenderRepository, SenderRepositoryError, SenderStats,
};

use crate::sender::{SmtpConfig, SmtpSender};

/// Internal abstraction that mirrors `EmailSender` but takes `email` by
/// reference.  This lets `MultiSmtpSender` iterate over senders without
/// consuming `OutboundEmail` on the first attempt.
pub(crate) trait SendRef: Send + Sync {
    fn send_ref<'a>(
        &'a self,
        email: &'a OutboundEmail,
    ) -> impl std::future::Future<Output = Result<SenderName, SendError>> + Send + 'a;
}

impl SendRef for SmtpSender {
    async fn send_ref<'a>(&'a self, email: &'a OutboundEmail) -> Result<SenderName, SendError> {
        self.send_inner(email)
            .await
            .map_err(|source| SendError::Send {
                sender_name: self.name().clone(),
                source,
            })?;
        Ok(self.name().clone())
    }
}

/// A `SenderRepository` that always returns zero counts for all senders.
/// Used as the default type argument for `MultiSmtpSender` and in tests that
/// do not exercise quota logic.
pub struct NoopSenderRepository;

impl SenderRepository for NoopSenderRepository {
    async fn get_stats(
        &self,
        names: &[SenderName],
        _since_ms: i64,
    ) -> Result<Vec<SenderStats>, SenderRepositoryError> {
        Ok(names
            .iter()
            .map(|n| SenderStats {
                name: n.clone(),
                sent_in_range: 0,
                failed_in_range: 0,
            })
            .collect())
    }
}

struct SenderEntry<S> {
    sender: S,
    name: SenderName,
    quota: Option<SenderQuota>,
}

/// Tries each contained sender in priority order (ascending); falls through
/// to the next on `SendError`.  Returns the last error if all senders fail.
///
/// When a sender has a `SenderQuota`, the first pass skips it if its
/// `sent_in_range` has reached the quota limit.  If the first pass exhausts
/// all eligible senders, a second pass tries every sender regardless of quota
/// so that traffic is never dropped outright.
pub struct MultiSmtpSender<S = SmtpSender, R = NoopSenderRepository> {
    /// Sorted by `priority` ascending at construction time; index 0 is tried
    /// first.
    senders: Vec<SenderEntry<S>>,
    repo: R,
}

impl<S: SendRef, R: SenderRepository> EmailSender for MultiSmtpSender<S, R> {
    async fn send(&self, email: OutboundEmail) -> Result<SenderName, SendError> {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));

        let range_to_since = |range: &QuotaRange| -> i64 {
            let offset_ms: i64 = match range {
                QuotaRange::Hourly => 3_600_000,
                QuotaRange::Daily => 86_400_000,
                QuotaRange::Weekly => 604_800_000,
                QuotaRange::Monthly => 2_592_000_000,
            };
            now_ms.saturating_sub(offset_ms).max(0)
        };

        // Build a map: since_ms -> Vec<SenderName>.
        let mut since_to_names: HashMap<i64, Vec<SenderName>> = HashMap::new();
        for entry in &self.senders {
            if let Some(quota) = &entry.quota {
                let since_ms = range_to_since(&quota.range);
                since_to_names
                    .entry(since_ms)
                    .or_default()
                    .push(entry.name.clone());
            }
        }

        // Query repo for each unique since_ms group, aggregate results.
        let mut sent_map: HashMap<SenderName, u64> = HashMap::new();
        let mut repo_ok = true;
        for (since_ms, names) in &since_to_names {
            if let Ok(stats) = self.repo.get_stats(names, *since_ms).await {
                for stat in stats {
                    sent_map.insert(stat.name, stat.sent_in_range);
                }
            } else {
                repo_ok = false;
                break;
            }
        }

        // Helper: is a sender over quota?
        let is_over_quota = |entry: &SenderEntry<S>| -> bool {
            if !repo_ok {
                return false;
            }
            match &entry.quota {
                None => false,
                Some(quota) => {
                    let sent = sent_map.get(&entry.name).copied().unwrap_or(0);
                    sent >= quota.count
                }
            }
        };

        let mut last_err: Option<SendError> = None;
        for entry in &self.senders {
            if is_over_quota(entry) {
                continue;
            }
            match entry.sender.send_ref(&email).await {
                Ok(name) => return Ok(name),
                Err(err) => {
                    last_err = Some(err);
                }
            }
        }

        // Only needed when first pass found no success (either all eligible senders
        // failed, or all senders were over quota).
        let mut last_err_fallback: Option<SendError> = None;
        for entry in &self.senders {
            match entry.sender.send_ref(&email).await {
                Ok(name) => return Ok(name),
                Err(err) => {
                    last_err_fallback = Some(err);
                }
            }
        }

        // Return the last error from whichever pass produced one.
        Err(last_err_fallback
            .or(last_err)
            .expect("senders list must not be empty"))
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

    /// Builds a `MultiSmtpSender` from the configuration, using the given
    /// repository for quota stat lookups.
    ///
    /// # Errors
    ///
    /// Returns an error if any SMTP transport cannot be constructed.
    pub fn build<R: SenderRepository>(
        self,
        repo: R,
    ) -> anyhow::Result<MultiSmtpSender<SmtpSender, R>> {
        let entries = self
            .senders
            .into_iter()
            .map(|cfg| {
                let name = cfg.name.clone();
                let quota = cfg.quota;
                let sender = cfg.smtp.build_named(cfg.name)?;
                Ok(SenderEntry {
                    sender,
                    name,
                    quota,
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok(MultiSmtpSender {
            senders: entries,
            repo,
        })
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

    use catapulte_domain::entity::body::{Plain, RenderedBody};
    use catapulte_domain::entity::email::RecipientKind;
    use catapulte_domain::entity::sender::{QuotaRange, SenderName, SenderQuota};
    use catapulte_domain::port::email_sender::{EmailSender, OutboundEmail, SendError};
    use catapulte_domain::port::sender_repository::{
        SenderRepository, SenderRepositoryError, SenderStats,
    };

    use super::{MultiSenderConfig, MultiSmtpSender, NoopSenderRepository, SendRef, SenderEntry};

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

    enum FakeSender {
        Fail { name: SenderName },
        Ok { name: SenderName },
    }

    impl SendRef for FakeSender {
        async fn send_ref<'a>(
            &'a self,
            _email: &'a OutboundEmail,
        ) -> Result<SenderName, SendError> {
            match self {
                Self::Fail { name } => Err(SendError::Send {
                    sender_name: name.clone(),
                    source: anyhow::anyhow!("simulated failure"),
                }),
                Self::Ok { name } => Ok(name.clone()),
            }
        }
    }

    impl EmailSender for FakeSender {
        async fn send(&self, email: OutboundEmail) -> Result<SenderName, SendError> {
            self.send_ref(&email).await
        }
    }

    fn make_multi_noop(
        senders: Vec<(FakeSender, Option<SenderQuota>)>,
    ) -> MultiSmtpSender<FakeSender, NoopSenderRepository> {
        let entries = senders
            .into_iter()
            .map(|(sender, quota)| SenderEntry {
                name: match &sender {
                    FakeSender::Ok { name } | FakeSender::Fail { name } => name.clone(),
                },
                quota,
                sender,
            })
            .collect();
        MultiSmtpSender {
            senders: entries,
            repo: NoopSenderRepository,
        }
    }

    struct FakeSenderRepository {
        stats: HashMap<String, u64>, // name -> sent_in_range
    }

    impl SenderRepository for FakeSenderRepository {
        async fn get_stats(
            &self,
            names: &[SenderName],
            _since_ms: i64,
        ) -> Result<Vec<SenderStats>, SenderRepositoryError> {
            Ok(names
                .iter()
                .map(|n| SenderStats {
                    name: n.clone(),
                    sent_in_range: *self.stats.get(n.as_str()).unwrap_or(&0),
                    failed_in_range: 0,
                })
                .collect())
        }
    }

    struct FailingRepo;

    impl SenderRepository for FailingRepo {
        async fn get_stats(
            &self,
            _names: &[SenderName],
            _since_ms: i64,
        ) -> Result<Vec<SenderStats>, SenderRepositoryError> {
            Err(SenderRepositoryError::Storage {
                source: anyhow::anyhow!("db down"),
            })
        }
    }

    fn make_multi_with_repo<R: SenderRepository>(
        senders: Vec<(FakeSender, Option<SenderQuota>)>,
        repo: R,
    ) -> MultiSmtpSender<FakeSender, R> {
        let entries = senders
            .into_iter()
            .map(|(sender, quota)| SenderEntry {
                name: match &sender {
                    FakeSender::Ok { name } | FakeSender::Fail { name } => name.clone(),
                },
                quota,
                sender,
            })
            .collect();
        MultiSmtpSender {
            senders: entries,
            repo,
        }
    }

    #[tokio::test]
    async fn first_sender_fails_second_succeeds() {
        let multi = make_multi_noop(vec![
            (
                FakeSender::Fail {
                    name: SenderName::new("first"),
                },
                None,
            ),
            (
                FakeSender::Ok {
                    name: SenderName::new("second"),
                },
                None,
            ),
        ]);
        let result = multi.send(make_email()).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap().as_str(), "second");
    }

    #[tokio::test]
    async fn all_senders_fail_returns_last_error() {
        let multi = make_multi_noop(vec![
            (
                FakeSender::Fail {
                    name: SenderName::new("first"),
                },
                None,
            ),
            (
                FakeSender::Fail {
                    name: SenderName::new("second"),
                },
                None,
            ),
        ]);
        let result = multi.send(make_email()).await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().sender_name().as_str(), "second");
    }

    #[tokio::test]
    async fn first_sender_succeeds_returns_immediately() {
        let multi = make_multi_noop(vec![
            (
                FakeSender::Ok {
                    name: SenderName::new("first"),
                },
                None,
            ),
            (
                FakeSender::Ok {
                    name: SenderName::new("second"),
                },
                None,
            ),
        ]);
        let result = multi.send(make_email()).await;
        assert_eq!(result.unwrap().as_str(), "first");
    }

    #[tokio::test]
    async fn over_quota_sender_is_skipped_for_eligible_one() {
        // Primary has quota=1, sent=1 (at limit) -> backup (no quota) is used.
        let mut stats = HashMap::new();
        stats.insert("primary".to_string(), 1u64);
        let repo = FakeSenderRepository { stats };

        let multi = make_multi_with_repo(
            vec![
                (
                    FakeSender::Ok {
                        name: SenderName::new("primary"),
                    },
                    Some(SenderQuota {
                        count: 1,
                        range: QuotaRange::Daily,
                    }),
                ),
                (
                    FakeSender::Ok {
                        name: SenderName::new("backup"),
                    },
                    None,
                ),
            ],
            repo,
        );

        let result = multi.send(make_email()).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap().as_str(), "backup");
    }

    #[tokio::test]
    async fn all_over_quota_still_delivers_via_fallback() {
        // Both primary and backup are over quota.
        // First pass skips both; second pass tries primary -> primary returns Ok.
        let mut stats = HashMap::new();
        stats.insert("primary".to_string(), 1u64);
        stats.insert("backup".to_string(), 1u64);
        let repo = FakeSenderRepository { stats };

        let multi = make_multi_with_repo(
            vec![
                (
                    FakeSender::Ok {
                        name: SenderName::new("primary"),
                    },
                    Some(SenderQuota {
                        count: 1,
                        range: QuotaRange::Daily,
                    }),
                ),
                (
                    FakeSender::Ok {
                        name: SenderName::new("backup"),
                    },
                    Some(SenderQuota {
                        count: 1,
                        range: QuotaRange::Daily,
                    }),
                ),
            ],
            repo,
        );

        let result = multi.send(make_email()).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap().as_str(), "primary");
    }

    #[tokio::test]
    async fn under_quota_sender_is_used_normally() {
        // Primary has quota=10, sent=5 -> primary is used.
        let mut stats = HashMap::new();
        stats.insert("primary".to_string(), 5u64);
        let repo = FakeSenderRepository { stats };

        let multi = make_multi_with_repo(
            vec![(
                FakeSender::Ok {
                    name: SenderName::new("primary"),
                },
                Some(SenderQuota {
                    count: 10,
                    range: QuotaRange::Hourly,
                }),
            )],
            repo,
        );

        let result = multi.send(make_email()).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap().as_str(), "primary");
    }

    #[tokio::test]
    async fn repo_error_treats_all_as_eligible() {
        // FailingRepo returns Err -> sender is attempted anyway.
        let multi = make_multi_with_repo(
            vec![(
                FakeSender::Ok {
                    name: SenderName::new("primary"),
                },
                Some(SenderQuota {
                    count: 1,
                    range: QuotaRange::Daily,
                }),
            )],
            FailingRepo,
        );

        let result = multi.send(make_email()).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap().as_str(), "primary");
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
