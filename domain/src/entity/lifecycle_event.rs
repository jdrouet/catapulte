use crate::entity::email::EmailId;

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
    },
    Retrying {
        id: EmailId,
        attempt: u32,
        reason: String,
    },
    Failed {
        id: EmailId,
        reason: String,
    },
}
