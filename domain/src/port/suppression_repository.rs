use std::collections::HashSet;

use thiserror::Error;

use crate::entity::suppression::{Suppression, SuppressionReason};

#[derive(Debug, Error)]
pub enum SuppressionRepositoryError {
    #[error("suppression storage failed")]
    Storage {
        #[source]
        source: anyhow::Error,
    },
}

#[derive(Clone, Debug)]
pub struct ListSuppressionsParams {
    pub reason: Option<SuppressionReason>,
    /// Substring of the stored address.
    pub address: Option<String>,
    pub limit: u32,
    pub offset: u32,
}

pub trait SuppressionRepository: Send + Sync + 'static {
    /// # Errors
    ///
    /// Returns `SuppressionRepositoryError::Storage` when the query fails.
    fn list(
        &self,
        params: ListSuppressionsParams,
    ) -> impl std::future::Future<Output = Result<Vec<Suppression>, SuppressionRepositoryError>> + Send;

    /// Adds the entries; an address that is already suppressed keeps its first entry.
    ///
    /// # Errors
    ///
    /// Returns `SuppressionRepositoryError::Storage` when the write fails.
    fn add(
        &self,
        entries: &[Suppression],
    ) -> impl std::future::Future<Output = Result<(), SuppressionRepositoryError>> + Send;

    /// Returns `true` when an entry existed and was removed.
    ///
    /// # Errors
    ///
    /// Returns `SuppressionRepositoryError::Storage` when the write fails.
    fn delete(
        &self,
        address: &str,
    ) -> impl std::future::Future<Output = Result<bool, SuppressionRepositoryError>> + Send;

    /// Returns those of `addresses` (already lowercased bare addresses) that are suppressed.
    ///
    /// # Errors
    ///
    /// Returns `SuppressionRepositoryError::Storage` when the query fails.
    fn find_suppressed(
        &self,
        addresses: &[String],
    ) -> impl std::future::Future<Output = Result<HashSet<String>, SuppressionRepositoryError>> + Send;
}

/// Suppresses nothing; the default when no suppression list is wired in.
pub struct NoopSuppressionList;

impl SuppressionRepository for NoopSuppressionList {
    async fn list(
        &self,
        _params: ListSuppressionsParams,
    ) -> Result<Vec<Suppression>, SuppressionRepositoryError> {
        Ok(vec![])
    }

    async fn add(&self, _entries: &[Suppression]) -> Result<(), SuppressionRepositoryError> {
        Ok(())
    }

    async fn delete(&self, _address: &str) -> Result<bool, SuppressionRepositoryError> {
        Ok(false)
    }

    async fn find_suppressed(
        &self,
        _addresses: &[String],
    ) -> Result<HashSet<String>, SuppressionRepositoryError> {
        Ok(HashSet::new())
    }
}
