use crate::entity::email::EmailId;
use crate::entity::error_class::ErrorClass;
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
        error_class: ErrorClass,
        sender_name: Option<SenderName>,
        correlation_id: Option<String>,
    },
    Failed {
        id: EmailId,
        attempt: u32,
        reason: String,
        error_class: ErrorClass,
        sender_name: Option<SenderName>,
        correlation_id: Option<String>,
    },
}

impl LifecycleEvent {
    /// The public lifecycle event type string (stable wire contract).
    #[must_use]
    pub fn event_type(&self) -> &'static str {
        match self {
            Self::Queued { .. } => "queued",
            Self::Sending { .. } => "sending",
            Self::Sent { .. } => "delivery.succeeded",
            Self::Retrying { .. } => "retrying",
            Self::Failed { .. } => "delivery.failed",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::email::EmailId;
    use crate::entity::error_class::ErrorClass;
    use crate::entity::sender::SenderName;

    #[test]
    fn event_type_queued() {
        let e = LifecycleEvent::Queued {
            id: EmailId::default(),
            correlation_id: None,
        };
        assert_eq!(e.event_type(), "queued");
    }

    #[test]
    fn event_type_sending() {
        let e = LifecycleEvent::Sending {
            id: EmailId::default(),
            attempt: 1,
            correlation_id: None,
        };
        assert_eq!(e.event_type(), "sending");
    }

    #[test]
    fn event_type_sent() {
        let e = LifecycleEvent::Sent {
            id: EmailId::default(),
            sender_name: SenderName::new("primary"),
            correlation_id: None,
        };
        assert_eq!(e.event_type(), "delivery.succeeded");
    }

    #[test]
    fn event_type_retrying() {
        let e = LifecycleEvent::Retrying {
            id: EmailId::default(),
            attempt: 1,
            reason: "timeout".to_owned(),
            error_class: ErrorClass::Delivery,
            sender_name: None,
            correlation_id: None,
        };
        assert_eq!(e.event_type(), "retrying");
    }

    #[test]
    fn event_type_failed() {
        let e = LifecycleEvent::Failed {
            id: EmailId::default(),
            attempt: 3,
            reason: "smtp error".to_owned(),
            error_class: ErrorClass::Delivery,
            sender_name: None,
            correlation_id: None,
        };
        assert_eq!(e.event_type(), "delivery.failed");
    }
}
