use std::str::FromStr;

use thiserror::Error;

/// Fixed vocabulary describing why a send failed; never carries relay text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureCategory {
    Connection,
    Timeout,
    Tls,
    PermanentRejection,
    TemporaryRejection,
    Other,
}

impl FailureCategory {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Connection => "connection",
            Self::Timeout => "timeout",
            Self::Tls => "tls",
            Self::PermanentRejection => "permanent_rejection",
            Self::TemporaryRejection => "temporary_rejection",
            Self::Other => "other",
        }
    }
}

#[derive(Debug, Error)]
#[error("unknown failure category {value:?}")]
pub struct UnknownFailureCategory {
    pub value: String,
}

impl FromStr for FailureCategory {
    type Err = UnknownFailureCategory;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "connection" => Ok(Self::Connection),
            "timeout" => Ok(Self::Timeout),
            "tls" => Ok(Self::Tls),
            "permanent_rejection" => Ok(Self::PermanentRejection),
            "temporary_rejection" => Ok(Self::TemporaryRejection),
            "other" => Ok(Self::Other),
            _ => Err(UnknownFailureCategory {
                value: s.to_owned(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::FailureCategory;

    #[test]
    fn round_trips_every_variant_and_rejects_unknown() {
        for c in [
            FailureCategory::Connection,
            FailureCategory::Timeout,
            FailureCategory::Tls,
            FailureCategory::PermanentRejection,
            FailureCategory::TemporaryRejection,
            FailureCategory::Other,
        ] {
            assert_eq!(FailureCategory::from_str(c.as_str()).unwrap(), c);
        }
        assert!(FailureCategory::from_str("bogus").is_err());
    }
}
