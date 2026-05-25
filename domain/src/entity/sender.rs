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
}
