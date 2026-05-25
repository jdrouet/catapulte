use thiserror::Error;

use crate::port::email_repository::{
    EmailRecord, EmailRepository, EmailRepositoryError, ListEmailsParams,
};

#[derive(Debug, Error)]
pub enum ListEmailsError {
    #[error(transparent)]
    Repository(#[from] EmailRepositoryError),
}

pub trait ListEmailsUseCase: Send + Sync + 'static {
    /// # Errors
    ///
    /// Returns `ListEmailsError::Repository` when the underlying query fails.
    fn execute(
        &self,
        params: ListEmailsParams,
    ) -> impl std::future::Future<Output = Result<Vec<EmailRecord>, ListEmailsError>> + Send;
}

pub struct ListEmailsService<R> {
    repo: R,
}

impl<R> ListEmailsService<R> {
    pub fn new(repo: R) -> Self {
        Self { repo }
    }

    async fn execute_inner(
        &self,
        params: ListEmailsParams,
    ) -> Result<Vec<EmailRecord>, ListEmailsError>
    where
        R: EmailRepository,
    {
        Ok(self.repo.list_emails(params).await?)
    }
}

impl<R: EmailRepository + Send + Sync + 'static> ListEmailsUseCase for ListEmailsService<R> {
    fn execute(
        &self,
        params: ListEmailsParams,
    ) -> impl std::future::Future<Output = Result<Vec<EmailRecord>, ListEmailsError>> + Send {
        self.execute_inner(params)
    }
}
