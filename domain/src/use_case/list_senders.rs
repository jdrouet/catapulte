use std::collections::HashMap;

use thiserror::Error;

use crate::entity::sender::{SenderConfig, SenderName};
use crate::port::clock::Clock;
use crate::port::sender_usage::{SenderStats, SenderUsage as SenderUsagePort, SenderUsageError};

#[derive(Clone, Debug)]
pub struct SenderUsage {
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
    ) -> impl std::future::Future<Output = Result<Vec<SenderUsage>, ListSendersError>> + Send;
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

    async fn execute_inner(&self) -> Result<Vec<SenderUsage>, ListSendersError>
    where
        U: SenderUsagePort,
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
                SenderUsage {
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
    U: SenderUsagePort,
    C: Clock,
{
    fn execute(
        &self,
    ) -> impl std::future::Future<Output = Result<Vec<SenderUsage>, ListSendersError>> + Send {
        self.execute_inner()
    }
}
