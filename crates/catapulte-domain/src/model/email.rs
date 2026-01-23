/// A recipient email address with optional display name
#[derive(Debug, Clone)]
pub struct Recipient {
    pub name: Option<String>,
    pub email: String,
}

impl Recipient {
    pub fn new(email: impl Into<String>) -> Self {
        Self {
            name: None,
            email: email.into(),
        }
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }
}

/// Collection of recipients for an email
#[derive(Debug, Clone, Default)]
pub struct Recipients {
    pub to: Vec<Recipient>,
    pub cc: Vec<Recipient>,
    pub bcc: Vec<Recipient>,
}

impl Recipients {
    pub fn is_empty(&self) -> bool {
        self.to.is_empty() && self.cc.is_empty() && self.bcc.is_empty()
    }
}

/// An email attachment
#[derive(Debug, Clone)]
pub struct Attachment {
    pub filename: String,
    pub content_type: String,
    pub content: Vec<u8>,
}

/// An email request to be sent
#[derive(Debug, Clone)]
pub struct Email {
    pub template_name: String,
    pub from: Recipient,
    pub recipients: Recipients,
    pub params: serde_json::Value,
    pub attachments: Vec<Attachment>,
}

/// A rendered email ready for sending
#[derive(Debug, Clone)]
pub struct RenderedEmail {
    pub subject: String,
    pub text_body: Option<String>,
    pub html_body: String,
}
