use super::manager::TemplateManagerError;
use super::provider::TemplateProviderError;
use super::template::TemplateError;

#[derive(Clone, Debug)]
pub enum Error {
    Provider(TemplateProviderError),
    Manager(TemplateManagerError),
    Template(TemplateError),
}

impl From<TemplateProviderError> for Error {
    fn from(error: TemplateProviderError) -> Self {
        Error::Provider(error)
    }
}

impl From<TemplateManagerError> for Error {
    fn from(error: TemplateManagerError) -> Self {
        Error::Manager(error)
    }
}

impl From<TemplateError> for Error {
    fn from(error: TemplateError) -> Self {
        Error::Template(error)
    }
}
