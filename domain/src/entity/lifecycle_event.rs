use crate::entity::email::EmailId;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LifecycleEvent {
    Sent { id: EmailId },
}
