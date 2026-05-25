use thiserror::Error;

use crate::entity::sender::SenderName;

#[derive(Clone, Debug)]
pub struct SenderStats {
    pub name: SenderName,
    pub sent_in_range: u64,
    pub failed_in_range: u64,
}

#[derive(Debug, Error)]
pub enum SenderRepositoryError {
    #[error("sender repository query failed")]
    Storage {
        #[source]
        source: anyhow::Error,
    },
}

pub trait SenderRepository: Send + Sync + 'static {
    /// Returns stats for the given sender names, counting events since `since_ms`.
    /// Senders with no events in range are included with zero counts.
    ///
    /// # Errors
    ///
    /// Returns `SenderRepositoryError::Storage` when the query fails.
    fn get_stats(
        &self,
        names: &[SenderName],
        since_ms: i64,
    ) -> impl std::future::Future<Output = Result<Vec<SenderStats>, SenderRepositoryError>> + Send;
}
