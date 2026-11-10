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
}
