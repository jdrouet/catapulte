use thiserror::Error;

use crate::entity::suppression::Suppression;
use crate::port::suppression_repository::{
    ListSuppressionsParams, SuppressionRepository, SuppressionRepositoryError,
};

#[derive(Debug, Error)]
pub enum ManageSuppressionsError {
    #[error(transparent)]
    Repository(#[from] SuppressionRepositoryError),
}

pub trait ManageSuppressionsUseCase: Send + Sync + 'static {
    /// # Errors
    ///
    /// Returns `ManageSuppressionsError::Repository` when the query fails.
    fn list(
        &self,
        params: ListSuppressionsParams,
    ) -> impl std::future::Future<Output = Result<Vec<Suppression>, ManageSuppressionsError>> + Send;

    /// # Errors
    ///
    /// Returns `ManageSuppressionsError::Repository` when the write fails.
    fn add(
        &self,
        entries: Vec<Suppression>,
    ) -> impl std::future::Future<Output = Result<(), ManageSuppressionsError>> + Send;

    /// Returns `true` when the address was suppressed and is now removed.
    ///
    /// # Errors
    ///
    /// Returns `ManageSuppressionsError::Repository` when the write fails.
    fn remove(
        &self,
        address: String,
    ) -> impl std::future::Future<Output = Result<bool, ManageSuppressionsError>> + Send;
}

pub struct ManageSuppressionsService<R> {
    repo: R,
}

impl<R> ManageSuppressionsService<R> {
    pub fn new(repo: R) -> Self {
        Self { repo }
    }
}

impl<R: SuppressionRepository> ManageSuppressionsUseCase for ManageSuppressionsService<R> {
    async fn list(
        &self,
        params: ListSuppressionsParams,
    ) -> Result<Vec<Suppression>, ManageSuppressionsError> {
        Ok(self.repo.list(params).await?)
    }

    async fn add(&self, entries: Vec<Suppression>) -> Result<(), ManageSuppressionsError> {
        Ok(self.repo.add(&entries).await?)
    }

    async fn remove(&self, address: String) -> Result<bool, ManageSuppressionsError> {
        Ok(self.repo.delete(&address).await?)
    }
}
