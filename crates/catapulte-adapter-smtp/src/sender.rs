use anyhow::Context;
use catapulte_domain::error::SendError;
use catapulte_domain::model::{Email, Recipient, RenderedEmail};
use catapulte_domain::prelude::EmailSender;
use lettre::message::header::ContentType;
use lettre::message::{Mailbox, MultiPart, SinglePart};
use lettre::transport::smtp::AsyncSmtpTransport;
use lettre::{AsyncTransport, Message, Tokio1Executor};

use crate::config::SmtpConfig;

/// SMTP email sender implementing the `EmailSender` port
pub struct SmtpSender {
    transport: AsyncSmtpTransport<Tokio1Executor>,
}

impl SmtpSender {
    /// Create a new SMTP sender from configuration
    pub fn new(config: &SmtpConfig) -> Result<Self, SendError> {
        let transport = config
            .build()
            .map_err(|err| SendError::ConnectionError(anyhow::Error::new(err)))?;
        Ok(Self { transport })
    }

    fn recipient_to_mailbox(recipient: &Recipient) -> Result<Mailbox, SendError> {
        let address: lettre::Address = recipient
            .email
            .parse()
            .with_context(|| format!("invalid email address: {}", recipient.email))
            .map_err(SendError::BuildError)?;

        Ok(Mailbox::new(recipient.name.clone(), address))
    }

    fn build_message(email: &Email, rendered: &RenderedEmail) -> Result<Message, SendError> {
        let from = Self::recipient_to_mailbox(&email.from)?;

        let mut builder = Message::builder().from(from);

        for recipient in &email.recipients.to {
            builder = builder.to(Self::recipient_to_mailbox(recipient)?);
        }
        for recipient in &email.recipients.cc {
            builder = builder.cc(Self::recipient_to_mailbox(recipient)?);
        }
        for recipient in &email.recipients.bcc {
            builder = builder.bcc(Self::recipient_to_mailbox(recipient)?);
        }

        let multipart = match &rendered.text_body {
            Some(text) => {
                MultiPart::alternative_plain_html(text.clone(), rendered.html_body.clone())
            }
            None => {
                MultiPart::alternative().singlepart(SinglePart::html(rendered.html_body.clone()))
            }
        };

        let multipart = email.attachments.iter().fold(multipart, |mp, attachment| {
            let content_type: ContentType = attachment
                .content_type
                .parse()
                .unwrap_or(ContentType::TEXT_PLAIN);
            mp.singlepart(
                lettre::message::Attachment::new(attachment.filename.clone())
                    .body(attachment.content.clone(), content_type),
            )
        });

        builder
            .subject(&rendered.subject)
            .multipart(multipart)
            .map_err(|err| SendError::BuildError(anyhow::Error::new(err)))
    }
}

impl EmailSender for SmtpSender {
    async fn send(&self, email: &Email, rendered: &RenderedEmail) -> Result<(), SendError> {
        let message = Self::build_message(email, rendered)?;
        self.transport
            .send(message)
            .await
            .map_err(|err| SendError::TransportError(anyhow::Error::new(err)))?;
        Ok(())
    }

    async fn test_connection(&self) -> Result<(), SendError> {
        self.transport
            .test_connection()
            .await
            .map_err(|err| SendError::ConnectionError(anyhow::Error::new(err)))?;
        Ok(())
    }
}
