use crate::entity::email::EmailId;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LifecycleEvent {
    Sent { id: EmailId },
    Failed { id: EmailId, reason: String },
}
