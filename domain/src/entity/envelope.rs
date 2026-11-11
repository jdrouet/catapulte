use crate::entity::attachment::AttachmentRef;
use crate::entity::body::BodySource;
use crate::entity::email::RecipientKind;

/// List-Unsubscribe targets, validated at intake. At least one field is set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unsubscribe {
    pub url: Option<url::Url>,
    pub mailto: Option<String>,
}

impl Unsubscribe {
    /// Returns `local@ascii-domain` when the local part uses only `[A-Za-z0-9._+-]` and the
    /// domain is plain ASCII after punycode conversion; such a value is safe inside `<mailto:>`.
    #[must_use]
    pub fn normalize_mailto(raw: &str) -> Option<String> {
        let (local, domain) = raw.rsplit_once('@')?;
        if local.is_empty()
            || !local
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'+' | b'-'))
        {
            return None;
        }
        let domain = idna::domain_to_ascii(domain).ok()?.to_ascii_lowercase();
        let valid = !domain.is_empty()
            && domain
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-'));
        valid.then(|| format!("{local}@{domain}"))
    }

    /// The `List-Unsubscribe` header and, when a URL is set, the RFC 8058 one-click header.
    #[must_use]
    pub fn headers(&self) -> Vec<(&'static str, String)> {
        let targets: Vec<String> = self
            .url
            .iter()
            .map(|u| format!("<{u}>"))
            .chain(
                self.mailto
                    .iter()
                    .filter_map(|m| Self::normalize_mailto(m))
                    .map(|m| format!("<mailto:{m}>")),
            )
            .collect();
        if targets.is_empty() {
            return vec![];
        }
        let mut out = vec![("List-Unsubscribe", targets.join(", "))];
        if self.url.is_some() {
            out.push((
                "List-Unsubscribe-Post",
                "List-Unsubscribe=One-Click".to_owned(),
            ));
        }
        out
    }
}

#[derive(Clone)]
pub struct Envelope {
    /// Name of the API key that submitted the email; `""` for the legacy key, trusted mode
    /// and NATS. Idempotency keys are unique per owner.
    pub owner: String,
    pub idempotency_key: Option<String>,
    pub correlation_id: Option<String>,
    pub subject: Option<String>,
    pub sender: String,
    pub recipients: Vec<(RecipientKind, String)>,
    pub body: BodySource,
    pub variables: serde_json::Map<String, serde_json::Value>,
    pub attachments: Vec<AttachmentRef>,
    /// Canonical mailbox strings, validated at intake.
    pub reply_to: Vec<String>,
    /// Validated custom headers, in name order.
    pub headers: Vec<(String, String)>,
    pub unsubscribe: Option<Unsubscribe>,
}

impl Envelope {
    /// Builds an envelope with every optional field empty, for tests. Production code
    /// builds the struct literally so that a new field fails to compile until handled.
    #[must_use]
    pub fn new(
        sender: impl Into<String>,
        recipients: Vec<(RecipientKind, String)>,
        body: BodySource,
    ) -> Self {
        Self {
            owner: String::new(),
            idempotency_key: None,
            correlation_id: None,
            subject: None,
            sender: sender.into(),
            recipients,
            body,
            variables: serde_json::Map::new(),
            attachments: vec![],
            reply_to: vec![],
            headers: vec![],
            unsubscribe: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Unsubscribe;

    #[test]
    fn normalize_mailto_accepts_safe_addresses_and_punycodes_the_domain() {
        for ok in ["unsubscribe+list-1@example.com", "a.b_c-d@example.com"] {
            assert_eq!(Unsubscribe::normalize_mailto(ok).as_deref(), Some(ok));
        }
        assert_eq!(
            Unsubscribe::normalize_mailto("u@Bücher.de").as_deref(),
            Some("u@xn--bcher-kva.de")
        );
    }

    #[test]
    fn normalize_mailto_rejects_uri_delimiters_and_quoted_forms() {
        for bad in [
            "\"x>, <https://evil\"@e.com",
            "\"a b\"@e.com",
            "a?subject=x@e.com",
            "a%41@e.com",
            "a&b@e.com",
            "a,b@e.com",
            "@e.com",
            "noat",
        ] {
            assert_eq!(Unsubscribe::normalize_mailto(bad), None, "{bad}");
        }
    }

    #[test]
    fn headers_skip_an_unsafe_mailto_without_failing() {
        let unsafe_mailto = Unsubscribe {
            url: Some("https://example.com/u".parse().unwrap()),
            mailto: Some("a?x@e.com".to_owned()),
        };
        assert_eq!(unsafe_mailto.headers()[0].1, "<https://example.com/u>");

        let only_unsafe = Unsubscribe {
            url: None,
            mailto: Some("a%41@e.com".to_owned()),
        };
        assert!(only_unsafe.headers().is_empty());
    }
}
