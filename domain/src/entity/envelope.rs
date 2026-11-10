use crate::entity::attachment::AttachmentRef;
use crate::entity::body::BodySource;
use crate::entity::email::RecipientKind;

/// Sender and recipient strings are canonical mailbox strings (`Mailbox::to_string()`):
/// either `local@domain` or `Name <local@domain>`. Use `bare_address` to get the address.
#[derive(Clone)]
pub struct Envelope {
    pub idempotency_key: Option<String>,
    pub correlation_id: Option<String>,
    pub subject: Option<String>,
    pub sender: String,
    pub recipients: Vec<(RecipientKind, String)>,
    pub body: BodySource,
    pub variables: serde_json::Map<String, serde_json::Value>,
    pub attachments: Vec<AttachmentRef>,
    /// Canonical mailbox strings, validated at intake.
    pub reply_to: Vec<String>,
    /// Validated custom headers, in name order.
    pub headers: Vec<(String, String)>,
}

impl Envelope {
    /// Builds an envelope with every optional field empty, for tests. Production code
    /// builds the struct literally so that a new field fails to compile until handled.
    #[must_use]
    pub fn new(
        sender: impl Into<String>,
        recipients: Vec<(RecipientKind, String)>,
        body: BodySource,
    ) -> Self {
        Self {
            idempotency_key: None,
            correlation_id: None,
            subject: None,
            sender: sender.into(),
            recipients,
            body,
            variables: serde_json::Map::new(),
            attachments: vec![],
            reply_to: vec![],
            headers: vec![],
        }
    }
}
