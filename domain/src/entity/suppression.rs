use std::str::FromStr;

use thiserror::Error;

use crate::entity::email::{EmailId, bare_address};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SuppressionReason {
    Bounce,
    Complaint,
    Unsubscribe,
    Manual,
}

impl SuppressionReason {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Bounce => "bounce",
            Self::Complaint => "complaint",
            Self::Unsubscribe => "unsubscribe",
            Self::Manual => "manual",
        }
    }
}

#[derive(Debug, Error)]
#[error("unknown suppression reason {value:?}")]
pub struct UnknownSuppressionReason {
    pub value: String,
}

impl FromStr for SuppressionReason {
    type Err = UnknownSuppressionReason;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "bounce" => Ok(Self::Bounce),
            "complaint" => Ok(Self::Complaint),
            "unsubscribe" => Ok(Self::Unsubscribe),
            "manual" => Ok(Self::Manual),
            _ => Err(UnknownSuppressionReason {
                value: s.to_owned(),
            }),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Suppression {
    /// Lowercased bare address; see [`suppression_key`].
    pub address: String,
    pub reason: SuppressionReason,
    pub created_at_ms: i64,
    pub source_email_id: Option<EmailId>,
}

/// The key a recipient is matched on: the bare address of a mailbox, lowercased.
#[must_use]
pub fn suppression_key(mailbox: &str) -> String {
    bare_address(mailbox).to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_is_the_lowercased_bare_address() {
        assert_eq!(suppression_key("Ann <Ann@Example.COM>"), "ann@example.com");
        assert_eq!(suppression_key("a@b.c"), "a@b.c");
    }

    #[test]
    fn reasons_round_trip() {
        for r in [
            SuppressionReason::Bounce,
            SuppressionReason::Complaint,
            SuppressionReason::Unsubscribe,
            SuppressionReason::Manual,
        ] {
            assert_eq!(r.as_str().parse::<SuppressionReason>().unwrap(), r);
        }
        assert!("nope".parse::<SuppressionReason>().is_err());
    }
}
