/// Errors that can occur when loading a template
#[derive(Debug, thiserror::Error)]
pub enum TemplateLoadError {
    #[error("template not found: {name}")]
    NotFound { name: String },
    #[error("invalid template metadata")]
    InvalidMetadata(#[source] anyhow::Error),
    #[error("failed to read template")]
    IoError(#[source] anyhow::Error),
    #[error("failed to fetch remote template")]
    FetchError(#[source] anyhow::Error),
}

/// Errors that can occur when rendering a template
#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    #[error("failed to interpolate template variables")]
    Interpolation(#[source] anyhow::Error),
    #[error("failed to parse template")]
    Parse(#[source] anyhow::Error),
    #[error("failed to render template")]
    Render(#[source] anyhow::Error),
}

/// Errors that can occur when sending an email
#[derive(Debug, thiserror::Error)]
pub enum SendError {
    #[error("failed to build email message")]
    BuildError(#[source] anyhow::Error),
    #[error("failed to send email")]
    TransportError(#[source] anyhow::Error),
    #[error("connection error")]
    ConnectionError(#[source] anyhow::Error),
}

/// Combined error type for the email sending use case
#[derive(Debug, thiserror::Error)]
pub enum SendEmailError {
    #[error("no recipients specified")]
    NoRecipients,
    #[error("failed to load template")]
    TemplateLoad(#[source] TemplateLoadError),
    #[error("failed to render email")]
    Render(#[source] RenderError),
    #[error("failed to send email")]
    Send(#[source] SendError),
}

impl From<TemplateLoadError> for SendEmailError {
    fn from(err: TemplateLoadError) -> Self {
        SendEmailError::TemplateLoad(err)
    }
}

impl From<RenderError> for SendEmailError {
    fn from(err: RenderError) -> Self {
        SendEmailError::Render(err)
    }
}

impl From<SendError> for SendEmailError {
    fn from(err: SendError) -> Self {
        SendEmailError::Send(err)
    }
}
