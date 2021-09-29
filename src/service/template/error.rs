use super::manager::TemplateManagerError;
use super::template::TemplateError;

#[derive(Clone, Debug)]
pub enum Error {
    Manager(TemplateManagerError),
    Template(TemplateError),
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
