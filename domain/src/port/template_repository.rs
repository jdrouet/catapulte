use thiserror::Error;

/// One stored version of a named template.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredTemplate {
    pub name: String,
    pub version: u32,
    pub source: String,
    pub created_at_ms: i64,
}

#[derive(Debug, Error)]
pub enum TemplateRepositoryError {
    #[error("template storage failed")]
    Storage {
        #[source]
        source: anyhow::Error,
    },
}

pub trait TemplateRepository: Send + Sync + 'static {
    /// The newest version of `name`, if any.
    ///
    /// # Errors
    ///
    /// Returns `TemplateRepositoryError::Storage` when the query fails.
    fn latest(
        &self,
        name: &str,
    ) -> impl std::future::Future<Output = Result<Option<StoredTemplate>, TemplateRepositoryError>> + Send;

    /// The newest version of every template, ordered by name.
    ///
    /// # Errors
    ///
    /// Returns `TemplateRepositoryError::Storage` when the query fails.
    fn list(
        &self,
    ) -> impl std::future::Future<Output = Result<Vec<StoredTemplate>, TemplateRepositoryError>> + Send;

    /// Stores `source` as the next version of `name` and returns it.
    ///
    /// # Errors
    ///
    /// Returns `TemplateRepositoryError::Storage` when the write fails.
    fn put(
        &self,
        name: &str,
        source: &str,
        created_at_ms: i64,
    ) -> impl std::future::Future<Output = Result<StoredTemplate, TemplateRepositoryError>> + Send;

    /// Removes every version of `name`; `true` when something was removed.
    ///
    /// # Errors
    ///
    /// Returns `TemplateRepositoryError::Storage` when the write fails.
    fn delete(
        &self,
        name: &str,
    ) -> impl std::future::Future<Output = Result<bool, TemplateRepositoryError>> + Send;
}
