use crate::entity::email::EmailId;
use crate::entity::sender::SenderName;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LifecycleEvent {
    Queued {
        id: EmailId,
    },
    Sending {
        id: EmailId,
        attempt: u32,
    },
    Sent {
        id: EmailId,
        sender_name: SenderName,
    },
    Retrying {
        id: EmailId,
        attempt: u32,
        reason: String,
        sender_name: Option<SenderName>,
    },
    Failed {
        id: EmailId,
        reason: String,
        sender_name: Option<SenderName>,
    },
}
