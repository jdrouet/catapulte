use std::collections::BTreeMap;

use crate::entity::email::EmailId;
use crate::entity::error_class::ErrorClass;
use crate::entity::sender::SenderName;

/// Event types a worker emits; each proves the queue stored the message.
/// Informational events (`queued`, `suppressed`, `bounced`, `complained`) do not.
pub const WORKER_EVENT_TYPES: &[&str] = &[
    "sending",
    "retrying",
    "delivery.succeeded",
    "delivery.failed",
];

/// Event types that belong to the delivery pipeline. Only these decide whether an email can
/// still be delivered; any other event is informational.
pub const PIPELINE_EVENT_TYPES: &[&str] = &[
    "queued",
    "sending",
    "retrying",
    "delivery.succeeded",
    "delivery.failed",
];

/// Event types that settle an email's status; every other event is informational.
pub const TERMINAL_EVENT_TYPES: &[&str] = &["delivery.succeeded", "delivery.failed"];

/// What every event of one email carries besides its own fields.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EventContext {
    pub correlation_id: Option<String>,
    pub tags: Vec<String>,
    pub metadata: BTreeMap<String, String>,
}

impl EventContext {
    /// A context with a correlation id and no tags or metadata.
    #[must_use]
    pub fn with_correlation(correlation_id: Option<String>) -> Self {
        Self {
            correlation_id,
            ..Self::default()
        }
    }

    /// Writes the context into an event payload. `correlation_id` is always present;
    /// `tags` and `metadata` appear only when non-empty, so payloads of emails without them
    /// are unchanged.
    fn extend_payload(&self, payload: &mut serde_json::Value) {
        let Some(object) = payload.as_object_mut() else {
            return;
        };
        object.insert(
            "correlation_id".to_owned(),
            serde_json::json!(self.correlation_id),
        );
        if !self.tags.is_empty() {
            object.insert("tags".to_owned(), serde_json::json!(self.tags));
        }
        if !self.metadata.is_empty() {
            object.insert("metadata".to_owned(), serde_json::json!(self.metadata));
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LifecycleEvent {
    Queued {
        id: EmailId,
        context: EventContext,
    },
    Sending {
        id: EmailId,
        attempt: u32,
        context: EventContext,
    },
    Sent {
        id: EmailId,
        sender_name: SenderName,
        context: EventContext,
        /// The relay's reply to the accepted message, capped in length.
        smtp_response: Option<String>,
        message_id: String,
    },
    Retrying {
        id: EmailId,
        attempt: u32,
        reason: String,
        error_class: ErrorClass,
        sender_name: Option<SenderName>,
        context: EventContext,
    },
    Failed {
        id: EmailId,
        attempt: u32,
        reason: String,
        error_class: ErrorClass,
        sender_name: Option<SenderName>,
        context: EventContext,
    },
    /// Informational: recipients were dropped because they are on the suppression list.
    Suppressed {
        id: EmailId,
        addresses: Vec<String>,
        context: EventContext,
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
            Self::Suppressed { .. } => "suppressed",
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
            | Self::Failed { id, .. }
            | Self::Suppressed { id, .. } => id,
        }
    }

    /// The sender name, if this event carries one.
    ///
    /// `Queued` and `Sending` never have a sender; `Sent` always has one;
    /// `Retrying` and `Failed` carry an optional sender.
    #[must_use]
    pub fn sender_name(&self) -> Option<&SenderName> {
        match self {
            Self::Queued { .. } | Self::Sending { .. } | Self::Suppressed { .. } => None,
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
        let (mut payload, context) = match self {
            Self::Queued { context, .. } => (serde_json::json!({}), context),
            Self::Suppressed {
                addresses, context, ..
            } => (serde_json::json!({ "addresses": addresses }), context),
            Self::Sending {
                attempt, context, ..
            } => (serde_json::json!({ "attempt": attempt }), context),
            Self::Sent {
                sender_name,
                context,
                smtp_response,
                message_id,
                ..
            } => (
                serde_json::json!({
                    "sender_name": sender_name.as_str(),
                    "smtp_response": smtp_response,
                    "message_id": message_id,
                }),
                context,
            ),
            Self::Retrying {
                attempt,
                reason,
                error_class,
                sender_name,
                context,
                ..
            }
            | Self::Failed {
                attempt,
                reason,
                error_class,
                sender_name,
                context,
                ..
            } => (
                serde_json::json!({
                    "attempt": attempt,
                    "reason": reason,
                    "error_class": error_class.as_str(),
                    "sender_name": sender_name.as_ref().map(SenderName::as_str),
                }),
                context,
            ),
        };
        context.extend_payload(&mut payload);
        payload
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::email::EmailId;
    use crate::entity::error_class::ErrorClass;
    use crate::entity::sender::SenderName;

    #[test]
    fn suppressed_is_informational_for_status_and_worker_evidence() {
        let e = LifecycleEvent::Suppressed {
            id: EmailId::default(),
            addresses: vec!["a@x.com".to_owned()],
            context: EventContext::with_correlation(None),
        };
        assert_eq!(e.event_type(), "suppressed");
        assert!(!TERMINAL_EVENT_TYPES.contains(&e.event_type()));
        assert!(!WORKER_EVENT_TYPES.contains(&e.event_type()));
    }

    fn ctx() -> EventContext {
        EventContext::with_correlation(Some("c1".to_owned()))
    }

    #[test]
    fn payloads_without_tags_or_metadata_keep_their_original_shape() {
        let id = EmailId::default();
        let cases = [
            (
                LifecycleEvent::Queued { id, context: ctx() },
                serde_json::json!({ "correlation_id": "c1" }),
            ),
            (
                LifecycleEvent::Sending {
                    id,
                    attempt: 2,
                    context: ctx(),
                },
                serde_json::json!({ "attempt": 2, "correlation_id": "c1" }),
            ),
            (
                LifecycleEvent::Sent {
                    id,
                    sender_name: SenderName::new("p"),
                    context: ctx(),
                    smtp_response: Some("250 ok".to_owned()),
                    message_id: "<m@x>".to_owned(),
                },
                serde_json::json!({
                    "sender_name": "p", "correlation_id": "c1",
                    "smtp_response": "250 ok", "message_id": "<m@x>"
                }),
            ),
            (
                LifecycleEvent::Retrying {
                    id,
                    attempt: 1,
                    reason: "r".to_owned(),
                    error_class: ErrorClass::Delivery,
                    sender_name: None,
                    context: ctx(),
                },
                serde_json::json!({
                    "attempt": 1, "reason": "r", "error_class": "delivery",
                    "sender_name": null, "correlation_id": "c1"
                }),
            ),
            (
                LifecycleEvent::Failed {
                    id,
                    attempt: 3,
                    reason: "r".to_owned(),
                    error_class: ErrorClass::Rejected,
                    sender_name: Some(SenderName::new("p")),
                    context: ctx(),
                },
                serde_json::json!({
                    "attempt": 3, "reason": "r", "error_class": "rejected",
                    "sender_name": "p", "correlation_id": "c1"
                }),
            ),
            (
                LifecycleEvent::Suppressed {
                    id,
                    addresses: vec!["a@x.com".to_owned()],
                    context: ctx(),
                },
                serde_json::json!({ "addresses": ["a@x.com"], "correlation_id": "c1" }),
            ),
        ];
        for (event, expected) in cases {
            assert_eq!(event.payload(), expected, "{}", event.event_type());
        }
    }

    #[test]
    fn tags_and_metadata_appear_in_the_payload_only_when_set() {
        let mut context = ctx();
        context.tags = vec!["welcome".to_owned()];
        context.metadata.insert("order".to_owned(), "42".to_owned());
        let event = LifecycleEvent::Queued {
            id: EmailId::default(),
            context,
        };
        assert_eq!(
            event.payload(),
            serde_json::json!({
                "correlation_id": "c1", "tags": ["welcome"], "metadata": { "order": "42" }
            })
        );
    }

    #[test]
    fn event_type_queued() {
        let e = LifecycleEvent::Queued {
            id: EmailId::default(),
            context: EventContext::with_correlation(None),
        };
        assert_eq!(e.event_type(), "queued");
    }

    #[test]
    fn event_type_sending() {
        let e = LifecycleEvent::Sending {
            id: EmailId::default(),
            attempt: 1,
            context: EventContext::with_correlation(None),
        };
        assert_eq!(e.event_type(), "sending");
    }

    #[test]
    fn event_type_sent() {
        let e = LifecycleEvent::Sent {
            id: EmailId::default(),
            sender_name: SenderName::new("primary"),
            context: EventContext::with_correlation(None),
            smtp_response: None,
            message_id: "<test@example.com>".to_owned(),
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
            context: EventContext::with_correlation(None),
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
            context: EventContext::with_correlation(None),
        };
        assert_eq!(e.event_type(), "delivery.failed");
    }

    #[test]
    fn email_id_returns_id_field_for_all_variants() {
        let id = EmailId::default();
        let variants = [
            LifecycleEvent::Queued {
                id,
                context: EventContext::with_correlation(None),
            },
            LifecycleEvent::Sending {
                id,
                attempt: 1,
                context: EventContext::with_correlation(None),
            },
            LifecycleEvent::Sent {
                id,
                sender_name: SenderName::new("s"),
                context: EventContext::with_correlation(None),
                smtp_response: None,
                message_id: "<test@example.com>".to_owned(),
            },
            LifecycleEvent::Retrying {
                id,
                attempt: 1,
                reason: "r".to_owned(),
                error_class: ErrorClass::Delivery,
                sender_name: None,
                context: EventContext::with_correlation(None),
            },
            LifecycleEvent::Failed {
                id,
                attempt: 1,
                reason: "r".to_owned(),
                error_class: ErrorClass::Delivery,
                sender_name: None,
                context: EventContext::with_correlation(None),
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
                context: EventContext::default()
            }
            .sender_name()
            .is_none()
        );
        assert!(
            LifecycleEvent::Sending {
                id,
                attempt: 1,
                context: EventContext::default()
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
            context: EventContext::with_correlation(None),
            smtp_response: None,
            message_id: "<test@example.com>".to_owned(),
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
            context: EventContext::with_correlation(None),
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
            context: EventContext::with_correlation(None),
        };
        assert!(e.sender_name().is_none());
    }

    #[test]
    fn error_class_only_on_retrying_and_failed() {
        let id = EmailId::default();
        assert!(
            LifecycleEvent::Queued {
                id,
                context: EventContext::default()
            }
            .error_class()
            .is_none()
        );
        assert!(
            LifecycleEvent::Sending {
                id,
                attempt: 1,
                context: EventContext::default()
            }
            .error_class()
            .is_none()
        );
        assert!(
            LifecycleEvent::Sent {
                id,
                sender_name: SenderName::new("s"),
                context: EventContext::with_correlation(None),
                smtp_response: None,
                message_id: "<test@example.com>".to_owned(),
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
            context: EventContext::with_correlation(None),
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
            context: EventContext::with_correlation(None),
        };
        let expected = serde_json::json!({ "correlation_id": null });
        assert_eq!(e.payload(), expected);
    }

    #[test]
    fn payload_queued_with_correlation_id() {
        let id = EmailId::default();
        let e = LifecycleEvent::Queued {
            id,
            context: EventContext::with_correlation(Some("corr-123".to_owned())),
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
            context: EventContext::with_correlation(Some("c".to_owned())),
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
            context: EventContext::with_correlation(None),
            smtp_response: Some("250 2.0.0 Ok: queued as 4ABC".to_owned()),
            message_id: "<test@example.com>".to_owned(),
        };
        let expected = serde_json::json!({
            "sender_name": "primary",
            "correlation_id": null,
            "smtp_response": "250 2.0.0 Ok: queued as 4ABC",
            "message_id": "<test@example.com>",
        });
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
            context: EventContext::with_correlation(Some("c".to_owned())),
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
            context: EventContext::with_correlation(Some("corr-xyz".to_owned())),
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
            context: EventContext::with_correlation(None),
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
