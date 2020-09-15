use super::manager::TemplateManagerError;
use super::provider::TemplateProviderError;
use super::template::TemplateError;

#[derive(Clone, Debug)]
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
