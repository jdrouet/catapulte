crate::genid!(EmailId);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RecipientKind {
    To,
    Cc,
    Bcc,
}

/// Returns the bare `local@domain` part of a canonical mailbox string.
///
/// For `Name <local@domain>` this is the text after the last `<`; a string that
/// does not end with `>` is returned unchanged.
#[must_use]
pub fn bare_address(mailbox: &str) -> &str {
    if mailbox.ends_with('>')
        && let Some((_, rest)) = mailbox.rsplit_once('<')
    {
        return &rest[..rest.len() - 1];
    }
    mailbox
}

#[cfg(test)]
mod bare_address_tests {
    use super::bare_address;

    #[test]
    fn bare_address_extracts_angle_part() {
        assert_eq!(bare_address("Acme <noreply@acme.com>"), "noreply@acme.com");
        assert_eq!(bare_address("\"A <b>\" <a@b.c>"), "a@b.c");
        assert_eq!(bare_address("a@b.c"), "a@b.c");
        assert_eq!(bare_address("<a@b.c>"), "a@b.c");
        assert_eq!(bare_address("\"a b\"@example.com"), "\"a b\"@example.com");
        assert_eq!(bare_address("a@[127.0.0.1]"), "a@[127.0.0.1]");
        assert_eq!(bare_address("\"A <b>\" <a@[1.2.3.4]>"), "a@[1.2.3.4]");
    }
}
