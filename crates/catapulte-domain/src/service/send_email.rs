use crate::error::SendEmailError;
use crate::model::Email;
use crate::prelude::{EmailSender, TemplateLoader, TemplateRenderer};

/// Service that orchestrates the email sending use case
///
/// This service coordinates template loading, rendering, and sending.
/// It owns its dependencies directly - wrap the service in `Arc` for sharing.
pub struct SendEmailService<L, R, S> {
    loader: L,
    renderer: R,
    sender: S,
}

impl<L, R, S> SendEmailService<L, R, S>
where
    L: TemplateLoader,
    R: TemplateRenderer,
    S: EmailSender,
{
    pub fn new(loader: L, renderer: R, sender: S) -> Self {
        Self {
            loader,
            renderer,
            sender,
        }
    }

    /// Send an email using the configured template
    pub async fn send(&self, email: &Email) -> Result<(), SendEmailError> {
        if email.recipients.is_empty() {
            return Err(SendEmailError::NoRecipients);
        }

        let template = self.loader.load(&email.template_name).await?;
        let rendered = self.renderer.render(&template, &email.params).await?;
        self.sender.send(email, &rendered).await?;

        Ok(())
    }

    /// Test the connection to the email server
    pub async fn test_connection(&self) -> Result<(), SendEmailError> {
        self.sender.test_connection().await?;
        Ok(())
    }
}
