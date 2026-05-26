use crate::entity::email::EmailId;
use crate::entity::sender::SenderName;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LifecycleEvent {
    Queued {
        id: EmailId,
        correlation_id: Option<String>,
    },
    Sending {
        id: EmailId,
        attempt: u32,
        correlation_id: Option<String>,
    },
    Sent {
        id: EmailId,
        sender_name: SenderName,
        correlation_id: Option<String>,
    },
    Retrying {
        id: EmailId,
        attempt: u32,
        reason: String,
        sender_name: Option<SenderName>,
        correlation_id: Option<String>,
    },
    Failed {
        id: EmailId,
        attempt: u32,
        reason: String,
        sender_name: Option<SenderName>,
        correlation_id: Option<String>,
    },
}
