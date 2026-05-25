use std::collections::HashMap;

use thiserror::Error;

use crate::entity::sender::{SenderConfig, SenderName};
use crate::port::clock::Clock;
use crate::port::sender_usage::{SenderStats, SenderUsage, SenderUsageError};

#[derive(Clone, Debug)]
pub struct SenderSnapshot {
    pub config: SenderConfig,
    pub sent_in_range: u64,
    pub failed_in_range: u64,
}

#[derive(Debug, Error)]
pub enum ListSendersError {
    #[error("sender usage query failed")]
    Usage {
        #[source]
        source: SenderUsageError,
    },
}

pub trait ListSendersUseCase: Send + Sync + 'static {
    fn execute(
        &self,
    ) -> impl std::future::Future<Output = Result<Vec<SenderSnapshot>, ListSendersError>> + Send;
}

pub struct ListSendersService<U, C = crate::port::clock::SystemClock> {
    configs: Vec<SenderConfig>,
    usage: U,
    clock: C,
}

impl<U, C> ListSendersService<U, C> {
    pub fn new(configs: Vec<SenderConfig>, usage: U, clock: C) -> Self {
        Self {
            configs,
            usage,
            clock,
        }
    }

    async fn execute_inner(&self) -> Result<Vec<SenderSnapshot>, ListSendersError>
    where
        U: SenderUsage,
        C: Clock,
    {
        if self.configs.is_empty() {
            return Ok(vec![]);
        }

        let now_ms = self.clock.now_ms();

        let mut groups: HashMap<i64, Vec<SenderName>> = HashMap::new();
        for config in &self.configs {
            let since_ms = config
                .quota
                .as_ref()
                .map_or(0, |q| q.range.since_ms(now_ms));
            groups
                .entry(since_ms)
                .or_default()
                .push(config.name.clone());
        }

        let mut stats_map: HashMap<SenderName, SenderStats> = HashMap::new();
        for (since_ms, names) in &groups {
            let results = self
                .usage
                .get_stats(names, *since_ms)
                .await
                .map_err(|source| ListSendersError::Usage { source })?;
            for s in results {
                stats_map.insert(s.name.clone(), s);
            }
        }

        Ok(self
            .configs
            .iter()
            .map(|config| {
                let stats = stats_map.get(&config.name);
                SenderSnapshot {
                    config: config.clone(),
                    sent_in_range: stats.map_or(0, |s| s.sent_in_range),
                    failed_in_range: stats.map_or(0, |s| s.failed_in_range),
                }
            })
            .collect())
    }
}

impl<U, C> ListSendersUseCase for ListSendersService<U, C>
where
    U: SenderUsage,
    C: Clock,
{
    fn execute(
        &self,
    ) -> impl std::future::Future<Output = Result<Vec<SenderSnapshot>, ListSendersError>> + Send
    {
        self.execute_inner()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use crate::entity::sender::{QuotaRange, SenderConfig, SenderName, SenderQuota};
    use crate::port::clock::SystemClock;
    use crate::port::sender_usage::{SenderStats, SenderUsage, SenderUsageError};

    use super::{ListSendersError, ListSendersService, ListSendersUseCase};

    struct FakeSenderUsage {
        data: HashMap<String, SenderStats>,
        call_count: Arc<Mutex<usize>>,
        calls: Arc<Mutex<Vec<(Vec<SenderName>, i64)>>>,
        fail: bool,
    }

    impl FakeSenderUsage {
        fn new(data: HashMap<String, SenderStats>) -> Self {
            Self {
                data,
                call_count: Arc::new(Mutex::new(0)),
                calls: Arc::new(Mutex::new(vec![])),
                fail: false,
            }
        }

        fn failing() -> Self {
            Self {
                data: HashMap::new(),
                call_count: Arc::new(Mutex::new(0)),
                calls: Arc::new(Mutex::new(vec![])),
                fail: true,
            }
        }
    }

    #[allow(async_fn_in_trait)]
    impl SenderUsage for FakeSenderUsage {
        async fn get_stats(
            &self,
            names: &[SenderName],
            since_ms: i64,
        ) -> Result<Vec<SenderStats>, SenderUsageError> {
            *self.call_count.lock().unwrap() += 1;
            self.calls.lock().unwrap().push((names.to_vec(), since_ms));
            if self.fail {
                return Err(SenderUsageError::Storage {
                    source: anyhow::anyhow!("storage error"),
                });
            }
            let results = names
                .iter()
                .filter_map(|n| self.data.get(n.as_str()).cloned())
                .collect();
            Ok(results)
        }
    }

    fn config_no_quota(name: &str) -> SenderConfig {
        SenderConfig {
            name: SenderName::new(name),
            quota: None,
        }
    }

    fn config_with_quota(name: &str, range: QuotaRange) -> SenderConfig {
        SenderConfig {
            name: SenderName::new(name),
            quota: Some(SenderQuota { count: 100, range }),
        }
    }

    fn make_stats(name: &str, sent: u64, failed: u64) -> SenderStats {
        SenderStats {
            name: SenderName::new(name),
            sent_in_range: sent,
            failed_in_range: failed,
        }
    }

    #[tokio::test]
    async fn empty_configs_returns_empty_vec() {
        let usage = FakeSenderUsage::new(HashMap::new());
        let service = ListSendersService::new(vec![], usage, SystemClock);
        let result = service.execute().await.unwrap();
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn sender_without_quota_uses_since_ms_zero() {
        let mut data = HashMap::new();
        data.insert("alpha".to_owned(), make_stats("alpha", 5, 2));
        let usage = FakeSenderUsage::new(data);
        let calls = Arc::clone(&usage.calls);
        let service = ListSendersService::new(vec![config_no_quota("alpha")], usage, SystemClock);
        let result = service.execute().await.unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].sent_in_range, 5);
        assert_eq!(result[0].failed_in_range, 2);
        let locked = calls.lock().unwrap();
        assert_eq!(locked[0].1, 0);
    }

    #[tokio::test]
    async fn stats_default_to_zero_when_sender_missing_from_response() {
        let usage = FakeSenderUsage::new(HashMap::new());
        let service = ListSendersService::new(vec![config_no_quota("missing")], usage, SystemClock);
        let result = service.execute().await.unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].sent_in_range, 0);
        assert_eq!(result[0].failed_in_range, 0);
    }

    #[tokio::test]
    async fn senders_with_same_quota_range_batched_in_single_call() {
        let mut data = HashMap::new();
        data.insert("a".to_owned(), make_stats("a", 1, 0));
        data.insert("b".to_owned(), make_stats("b", 2, 0));
        let usage = FakeSenderUsage::new(data);
        let call_count = Arc::clone(&usage.call_count);
        let configs = vec![
            config_with_quota("a", QuotaRange::Daily),
            config_with_quota("b", QuotaRange::Daily),
        ];
        let service = ListSendersService::new(configs, usage, SystemClock);
        service.execute().await.unwrap();
        assert_eq!(*call_count.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn senders_with_different_quota_ranges_batched_separately() {
        let mut data = HashMap::new();
        data.insert("daily".to_owned(), make_stats("daily", 1, 0));
        data.insert("hourly".to_owned(), make_stats("hourly", 2, 0));
        let usage = FakeSenderUsage::new(data);
        let call_count = Arc::clone(&usage.call_count);
        let configs = vec![
            config_with_quota("daily", QuotaRange::Daily),
            config_with_quota("hourly", QuotaRange::Hourly),
        ];
        let service = ListSendersService::new(configs, usage, SystemClock);
        service.execute().await.unwrap();
        assert_eq!(*call_count.lock().unwrap(), 2);
    }

    #[tokio::test]
    async fn usage_error_propagates_as_list_senders_error() {
        let usage = FakeSenderUsage::failing();
        let service = ListSendersService::new(vec![config_no_quota("any")], usage, SystemClock);
        let err = service.execute().await.unwrap_err();
        assert!(matches!(err, ListSendersError::Usage { .. }));
    }
}
