use std::str::FromStr;

use thiserror::Error;

use crate::entity::email::{EmailId, bare_address};
use crate::use_case::submit_email::normalize_sender_domain;

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

/// The canonical form of an address: lowercase local part and the domain in lowercase
/// punycode, so `bücher`, `BÜCHER` and `xn--bcher-kva` are one address. `None` when the
/// domain is not a valid hostname.
#[must_use]
pub fn normalize_suppression_address(address: &str) -> Option<String> {
    let (local, domain) = address.trim().rsplit_once('@')?;
    let domain = normalize_sender_domain(domain)?;
    Some(format!("{}@{domain}", local.to_lowercase()))
}

/// The key a recipient is matched on: the canonical form of the bare address of a mailbox.
/// An address that cannot be normalised falls back to its lowercase form.
#[must_use]
pub fn suppression_key(mailbox: &str) -> String {
    let bare = bare_address(mailbox);
    normalize_suppression_address(bare).unwrap_or_else(|| bare.to_lowercase())
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
    fn idn_forms_of_one_address_share_a_key() {
        let forms = [
            "u@bücher.example",
            "U@BÜCHER.example",
            "u@xn--bcher-kva.example",
        ];
        for form in forms {
            assert_eq!(suppression_key(form), "u@xn--bcher-kva.example", "{form}");
        }
        assert_eq!(normalize_suppression_address("nodomain"), None);
        assert_eq!(normalize_suppression_address("u@bad_domain!"), None);
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
