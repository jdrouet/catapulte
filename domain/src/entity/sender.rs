use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SenderName(String);

impl SenderName {
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SenderName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QuotaRange {
    Hourly,
    Daily,
    Weekly,
    Monthly,
}

impl fmt::Display for QuotaRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Hourly => write!(f, "hourly"),
            Self::Daily => write!(f, "daily"),
            Self::Weekly => write!(f, "weekly"),
            Self::Monthly => write!(f, "monthly"),
        }
    }
}

impl QuotaRange {
    /// Returns the Unix-epoch millisecond timestamp that marks the start of this
    /// quota window relative to `now_ms`.
    #[must_use]
    pub fn since_ms(&self, now_ms: i64) -> i64 {
        let offset: i64 = match self {
            Self::Hourly => 3_600_000,
            Self::Daily => 86_400_000,
            Self::Weekly => 604_800_000,
            Self::Monthly => 2_592_000_000,
        };
        now_ms.saturating_sub(offset).max(0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SenderQuota {
    pub count: u64,
    pub range: QuotaRange,
}

#[cfg(test)]
mod tests {
    use super::{QuotaRange, SenderName, SenderQuota};

    #[test]
    fn sender_name_roundtrip() {
        let name = SenderName::new("primary");
        assert_eq!(name.as_str(), "primary");
        assert_eq!(name.to_string(), "primary");
    }

    #[test]
    fn sender_name_equality() {
        assert_eq!(SenderName::new("a"), SenderName::new("a"));
        assert_ne!(SenderName::new("a"), SenderName::new("b"));
    }

    #[test]
    fn quota_range_display() {
        assert_eq!(QuotaRange::Monthly.to_string(), "monthly");
    }

    #[test]
    fn sender_quota_fields() {
        let q = SenderQuota {
            count: 1000,
            range: QuotaRange::Daily,
        };
        assert_eq!(q.count, 1000);
        assert_eq!(q.range, QuotaRange::Daily);
    }

    #[test]
    fn since_ms_daily_subtracts_correct_offset() {
        assert_eq!(
            QuotaRange::Daily.since_ms(100_000_000),
            100_000_000 - 86_400_000
        );
    }

    #[test]
    fn since_ms_saturates_at_zero() {
        assert_eq!(QuotaRange::Hourly.since_ms(0), 0);
    }
}
