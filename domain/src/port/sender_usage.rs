use thiserror::Error;

use crate::entity::sender::SenderName;

#[derive(Clone, Debug)]
pub struct SenderStats {
    pub name: SenderName,
    pub sent_in_range: u64,
    pub failed_in_range: u64,
}

#[derive(Debug, Error)]
pub enum SenderUsageError {
    #[error("sender usage query failed")]
    Storage {
        #[source]
        source: anyhow::Error,
    },
}

pub trait SenderUsagePort: Send + Sync + 'static {
    fn get_stats(
        &self,
        names: &[SenderName],
        since_ms: i64,
    ) -> impl std::future::Future<Output = Result<Vec<SenderStats>, SenderUsageError>> + Send;
}
