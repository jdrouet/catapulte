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

    /// The id of the email this event belongs to.
    #[must_use]
    pub fn email_id(&self) -> &EmailId {
        match self {
            Self::Queued { id, .. }
            | Self::Sending { id, .. }
            | Self::Sent { id, .. }
            | Self::Retrying { id, .. }
            | Self::Failed { id, .. } => id,
        }
    }

    /// The sender name, if this event carries one.
    ///
    /// `Queued` and `Sending` never have a sender; `Sent` always has one;
    /// `Retrying` and `Failed` carry an optional sender.
    #[must_use]
    pub fn sender_name(&self) -> Option<&SenderName> {
        match self {
            Self::Queued { .. } | Self::Sending { .. } => None,
            Self::Sent { sender_name, .. } => Some(sender_name),
            Self::Retrying { sender_name, .. } | Self::Failed { sender_name, .. } => {
                sender_name.as_ref()
            }
        }
    }

    /// The error class, if this event represents a failure condition.
    #[must_use]
    pub fn error_class(&self) -> Option<&crate::entity::error_class::ErrorClass> {
        match self {
            Self::Retrying { error_class, .. } | Self::Failed { error_class, .. } => {
                Some(error_class)
            }
            _ => None,
        }
    }

    /// The canonical payload object for this event (stable wire contract).
    ///
    /// Always returns a JSON object — never null. The shape matches what the
    /// webhook and NATS publishers push to consumers.
    #[must_use]
    pub fn payload(&self) -> serde_json::Value {
        match self {
            Self::Queued { correlation_id, .. } => {
                serde_json::json!({ "correlation_id": correlation_id })
            }
            Self::Sending {
                attempt,
                correlation_id,
                ..
            } => serde_json::json!({ "attempt": attempt, "correlation_id": correlation_id }),
            Self::Sent {
                sender_name,
                correlation_id,
                ..
            } => serde_json::json!({
                "sender_name": sender_name.as_str(),
                "correlation_id": correlation_id,
            }),
            Self::Retrying {
                attempt,
                reason,
                error_class,
                sender_name,
                correlation_id,
                ..
            }
            | Self::Failed {
                attempt,
                reason,
                error_class,
                sender_name,
                correlation_id,
                ..
            } => serde_json::json!({
                "attempt": attempt,
                "reason": reason,
                "error_class": error_class.as_str(),
                "sender_name": sender_name.as_ref().map(SenderName::as_str),
                "correlation_id": correlation_id,
            }),
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

    #[test]
    fn email_id_returns_id_field_for_all_variants() {
        let id = EmailId::default();
        let variants = [
            LifecycleEvent::Queued {
                id,
                correlation_id: None,
            },
            LifecycleEvent::Sending {
                id,
                attempt: 1,
                correlation_id: None,
            },
            LifecycleEvent::Sent {
                id,
                sender_name: SenderName::new("s"),
                correlation_id: None,
            },
            LifecycleEvent::Retrying {
                id,
                attempt: 1,
                reason: "r".to_owned(),
                error_class: ErrorClass::Delivery,
                sender_name: None,
                correlation_id: None,
            },
            LifecycleEvent::Failed {
                id,
                attempt: 1,
                reason: "r".to_owned(),
                error_class: ErrorClass::Delivery,
                sender_name: None,
                correlation_id: None,
            },
        ];
        for e in &variants {
            assert_eq!(e.email_id(), &id);
        }
    }

    #[test]
    fn sender_name_queued_and_sending_return_none() {
        let id = EmailId::default();
        assert!(
            LifecycleEvent::Queued {
                id,
                correlation_id: None
            }
            .sender_name()
            .is_none()
        );
        assert!(
            LifecycleEvent::Sending {
                id,
                attempt: 1,
                correlation_id: None
            }
            .sender_name()
            .is_none()
        );
    }

    #[test]
    fn sender_name_sent_returns_some() {
        let id = EmailId::default();
        let e = LifecycleEvent::Sent {
            id,
            sender_name: SenderName::new("primary"),
            correlation_id: None,
        };
        assert_eq!(e.sender_name().map(SenderName::as_str), Some("primary"));
    }

    #[test]
    fn sender_name_failed_with_some_returns_some() {
        let id = EmailId::default();
        let e = LifecycleEvent::Failed {
            id,
            attempt: 1,
            reason: "r".to_owned(),
            error_class: ErrorClass::Delivery,
            sender_name: Some(SenderName::new("fallback")),
            correlation_id: None,
        };
        assert_eq!(e.sender_name().map(SenderName::as_str), Some("fallback"));
    }

    #[test]
    fn sender_name_retrying_without_sender_returns_none() {
        let id = EmailId::default();
        let e = LifecycleEvent::Retrying {
            id,
            attempt: 1,
            reason: "r".to_owned(),
            error_class: ErrorClass::Delivery,
            sender_name: None,
            correlation_id: None,
        };
        assert!(e.sender_name().is_none());
    }

    #[test]
    fn error_class_only_on_retrying_and_failed() {
        let id = EmailId::default();
        assert!(
            LifecycleEvent::Queued {
                id,
                correlation_id: None
            }
            .error_class()
            .is_none()
        );
        assert!(
            LifecycleEvent::Sending {
                id,
                attempt: 1,
                correlation_id: None
            }
            .error_class()
            .is_none()
        );
        assert!(
            LifecycleEvent::Sent {
                id,
                sender_name: SenderName::new("s"),
                correlation_id: None
            }
            .error_class()
            .is_none()
        );
        let ec = LifecycleEvent::Failed {
            id,
            attempt: 1,
            reason: "r".to_owned(),
            error_class: ErrorClass::Routing,
            sender_name: None,
            correlation_id: None,
        }
        .error_class()
        .map(ErrorClass::as_str);
        assert_eq!(ec, Some("routing"));
    }

    #[test]
    fn payload_queued_without_correlation_id() {
        let id = EmailId::default();
        let e = LifecycleEvent::Queued {
            id,
            correlation_id: None,
        };
        let expected = serde_json::json!({ "correlation_id": null });
        assert_eq!(e.payload(), expected);
    }

    #[test]
    fn payload_queued_with_correlation_id() {
        let id = EmailId::default();
        let e = LifecycleEvent::Queued {
            id,
            correlation_id: Some("corr-123".to_owned()),
        };
        let expected = serde_json::json!({ "correlation_id": "corr-123" });
        assert_eq!(e.payload(), expected);
    }

    #[test]
    fn payload_sending() {
        let id = EmailId::default();
        let e = LifecycleEvent::Sending {
            id,
            attempt: 2,
            correlation_id: Some("c".to_owned()),
        };
        let expected = serde_json::json!({ "attempt": 2, "correlation_id": "c" });
        assert_eq!(e.payload(), expected);
    }

    #[test]
    fn payload_sent() {
        let id = EmailId::default();
        let e = LifecycleEvent::Sent {
            id,
            sender_name: SenderName::new("primary"),
            correlation_id: None,
        };
        let expected = serde_json::json!({ "sender_name": "primary", "correlation_id": null });
        assert_eq!(e.payload(), expected);
    }

    #[test]
    fn payload_retrying() {
        let id = EmailId::default();
        let e = LifecycleEvent::Retrying {
            id,
            attempt: 1,
            reason: "timeout".to_owned(),
            error_class: ErrorClass::Delivery,
            sender_name: Some(SenderName::new("primary")),
            correlation_id: Some("c".to_owned()),
        };
        let expected = serde_json::json!({
            "attempt": 1,
            "reason": "timeout",
            "error_class": "delivery",
            "sender_name": "primary",
            "correlation_id": "c",
        });
        assert_eq!(e.payload(), expected);
    }

    #[test]
    fn payload_failed() {
        let id = EmailId::default();
        let e = LifecycleEvent::Failed {
            id,
            attempt: 3,
            reason: "smtp error".to_owned(),
            error_class: ErrorClass::Delivery,
            sender_name: Some(SenderName::new("primary")),
            correlation_id: Some("corr-xyz".to_owned()),
        };
        let expected = serde_json::json!({
            "attempt": 3,
            "reason": "smtp error",
            "error_class": "delivery",
            "sender_name": "primary",
            "correlation_id": "corr-xyz",
        });
        assert_eq!(e.payload(), expected);
    }

    #[test]
    fn payload_failed_without_sender_name() {
        let id = EmailId::default();
        let e = LifecycleEvent::Failed {
            id,
            attempt: 1,
            reason: "no route".to_owned(),
            error_class: ErrorClass::Routing,
            sender_name: None,
            correlation_id: None,
        };
        let expected = serde_json::json!({
            "attempt": 1,
            "reason": "no route",
            "error_class": "routing",
            "sender_name": null,
            "correlation_id": null,
        });
        assert_eq!(e.payload(), expected);
    }
}
