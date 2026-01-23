//! Port traits that adapters implement
//!
//! These traits define the interfaces between the domain layer and external systems.
//! Implementations are provided by adapter crates.

use std::future::Future;

use crate::error::{RenderError, SendError, TemplateLoadError};
use crate::model::{Email, RenderedEmail, Template};

/// Port for loading templates from storage
pub trait TemplateLoader: Send + Sync {
    /// Load a template by name
    fn load(&self, name: &str) -> impl Future<Output = Result<Template, TemplateLoadError>> + Send;
}

/// Port for rendering templates into email content
pub trait TemplateRenderer: Send + Sync {
    /// Render a template with the given parameters
    fn render(
        &self,
        template: &Template,
        params: &serde_json::Value,
    ) -> impl Future<Output = Result<RenderedEmail, RenderError>> + Send;
}

/// Port for sending emails
pub trait EmailSender: Send + Sync {
    /// Send a rendered email
    fn send(
        &self,
        email: &Email,
        rendered: &RenderedEmail,
    ) -> impl Future<Output = Result<(), SendError>> + Send;

    /// Test the connection to the email server
    fn test_connection(&self) -> impl Future<Output = Result<(), SendError>> + Send;
}
