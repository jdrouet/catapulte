use super::provider::TemplateProviderError;
use super::manager::TemplateManagerError;
use super::template::TemplateError;

#[derive(Debug)]
pub enum Error {
    ProviderError(TemplateProviderError),
    ManagerError(TemplateManagerError),
    TemplateError(TemplateError),
}

impl From<TemplateProviderError> for Error {
    fn from(error: TemplateProviderError) -> Self {
        Error::ProviderError(error)
    }
}

impl From<TemplateManagerError> for Error {
    fn from(error: TemplateManagerError) -> Self {
        Error::ManagerError(error)
    }
}

impl From<TemplateError> for Error {
    fn from(error: TemplateError) -> Self {
        Error::TemplateError(error)
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::ProviderError(err) => err.fmt(f),
            Error::ManagerError(err) => err.fmt(f),
            Error::TemplateError(err) => err.fmt(f),
        }
    }
}
