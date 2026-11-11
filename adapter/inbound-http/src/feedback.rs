//! Parses a forwarded DSN (RFC 3464) or ARF abuse report (RFC 5965) into a `FeedbackReport`.
//! Everything read here is untrusted; the use case decides what it is worth.

use catapulte_domain::entity::email::{EmailId, bare_address};
use catapulte_domain::use_case::ingest_feedback::{FeedbackReport, ReportKind, ReturnPathEvidence};
use mail_parser::{Message, MessageParser, MessagePart, MimeHeaders, PartType};

/// The message is not a report this service understands.
#[derive(Debug, PartialEq, Eq)]
pub struct NotAReport(&'static str);

impl std::fmt::Display for NotAReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

/// `Key: value` lines of one report block, with folded lines joined.
fn fields(block: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for line in block.lines() {
        if line.starts_with([' ', '\t']) {
            if let Some((_, value)) = out.last_mut() {
                value.push(' ');
                value.push_str(line.trim());
            }
        } else if let Some((key, value)) = line.split_once(':') {
            out.push((key.trim().to_ascii_lowercase(), value.trim().to_owned()));
        }
    }
    out
}

fn field<'a>(fields: &'a [(String, String)], key: &str) -> Option<&'a str> {
    fields
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

fn blocks(text: &str) -> Vec<Vec<(String, String)>> {
    text.replace("\r\n", "\n")
        .split("\n\n")
        .map(fields)
        .filter(|f| !f.is_empty())
        .collect()
}

fn part_text(part: &MessagePart<'_>) -> String {
    String::from_utf8_lossy(part.contents()).into_owned()
}

fn part_type(part: &MessagePart<'_>) -> (String, String) {
    part.content_type().map_or_else(
        || (String::new(), String::new()),
        |ct| {
            (
                ct.ctype().to_ascii_lowercase(),
                ct.subtype().unwrap_or_default().to_ascii_lowercase(),
            )
        },
    )
}

/// Header lines of the embedded original, whether it arrived as a message or as headers only.
fn original_headers(message: &Message<'_>) -> Vec<(String, String)> {
    for part in &message.parts {
        match (&part.body, part_type(part)) {
            (PartType::Message(inner), _) => {
                return inner
                    .headers_raw()
                    .map(|(k, v)| (k.to_ascii_lowercase(), v.trim().to_owned()))
                    .collect();
            }
            (_, (ctype, subtype)) if ctype == "text" && subtype == "rfc822-headers" => {
                return fields(&part_text(part).replace("\r\n", "\n"));
            }
            _ => {}
        }
    }
    Vec::new()
}

fn email_id_from_message_id(value: &str) -> Option<EmailId> {
    let local = value.trim().trim_start_matches('<').split('@').next()?;
    uuid::Uuid::parse_str(local).ok().map(EmailId::from)
}

/// Looks for `bounces+{uuid}-{tag}@` in the headers that can carry the return path.
fn return_path_evidence(headers: &[&str]) -> ReturnPathEvidence {
    let mut found: Vec<ReturnPathEvidence> = Vec::new();
    for value in headers {
        let lower = value.to_ascii_lowercase();
        let mut rest = lower.as_str();
        while let Some(at) = rest.find("bounces+") {
            let after = &rest[at + "bounces+".len()..];
            let token = after.split(['@', '>', ' ', ',', ';']).next().unwrap_or("");
            found.push(parse_token(token));
            rest = after;
        }
    }
    match found.as_slice() {
        [] => ReturnPathEvidence::Absent,
        [first, others @ ..] if others.iter().all(|o| o == first) => first.clone(),
        _ => ReturnPathEvidence::Malformed,
    }
}

fn parse_token(token: &str) -> ReturnPathEvidence {
    let parsed = (token.len() == 36 + 1 + 16)
        .then(|| token.split_at(36))
        .and_then(|(id, tag)| {
            let tag = tag.strip_prefix('-')?;
            let id = uuid::Uuid::parse_str(id).ok()?;
            Some(ReturnPathEvidence::Signed {
                id: EmailId::from(id),
                tag: tag.to_owned(),
            })
        });
    parsed.unwrap_or(ReturnPathEvidence::Malformed)
}

fn recipient_address(value: &str) -> String {
    let value = value.rsplit(';').next().unwrap_or(value).trim();
    bare_address(value).to_owned()
}

/// # Errors
///
/// Returns `NotAReport` when the message is not a delivery status or abuse report with a
/// recipient to act on.
pub fn parse_report(raw: &[u8]) -> Result<FeedbackReport, NotAReport> {
    let message = MessageParser::default()
        .parse(raw)
        .ok_or(NotAReport("not a message"))?;
    let root = message.root_part().content_type();
    let is_report = root.is_some_and(|ct| {
        ct.ctype().eq_ignore_ascii_case("multipart")
            && ct
                .subtype()
                .is_some_and(|s| s.eq_ignore_ascii_case("report"))
    });
    if !is_report {
        return Err(NotAReport("not a multipart/report message"));
    }
    let report_type = root
        .and_then(|ct| ct.attribute("report-type"))
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    let status_part = |subtype: &str| {
        message
            .parts
            .iter()
            .find(|p| part_type(p) == ("message".to_owned(), subtype.to_owned()))
    };

    let original = original_headers(&message);
    let original_value = |key: &str| {
        original
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    };

    let (kind, recipient) = match report_type.as_str() {
        "delivery-status" => {
            let part = status_part("delivery-status").ok_or(NotAReport("no delivery status"))?;
            let groups = blocks(&part_text(part));
            let group = groups
                .iter()
                .find(|g| field(g, "final-recipient").is_some())
                .ok_or(NotAReport("no Final-Recipient"))?;
            let action = field(group, "action")
                .unwrap_or_default()
                .to_ascii_lowercase();
            let status = field(group, "status").unwrap_or_default();
            let class = status.chars().next();
            let valid_status = status.split('.').count() == 3 && matches!(class, Some('4' | '5'));
            if !(action == "failed" || action == "delayed") || !valid_status {
                return Err(NotAReport("not a failure or delay notification"));
            }
            (
                ReportKind::Bounce {
                    hard: action == "failed" && class == Some('5'),
                    diagnostic_code: field(group, "diagnostic-code").map(str::to_owned),
                },
                recipient_address(field(group, "final-recipient").unwrap_or_default()),
            )
        }
        "feedback-report" => {
            let part = status_part("feedback-report").ok_or(NotAReport("no feedback report"))?;
            let groups = blocks(&part_text(part));
            let group = groups
                .iter()
                .find(|g| field(g, "feedback-type").is_some())
                .ok_or(NotAReport("no Feedback-Type"))?;
            if !field(group, "feedback-type").is_some_and(|t| t.eq_ignore_ascii_case("abuse")) {
                return Err(NotAReport("not an abuse report"));
            }
            let recipient = field(group, "original-rcpt-to")
                .or_else(|| original_value("to"))
                .map(recipient_address)
                .ok_or(NotAReport("no reported recipient"))?;
            (ReportKind::Complaint, recipient)
        }
        _ => return Err(NotAReport("unsupported report type")),
    };
    if recipient.is_empty() {
        return Err(NotAReport("no reported recipient"));
    }

    let outer: Vec<&str> = message
        .headers_raw()
        .filter(|(k, _)| {
            [
                "to",
                "delivered-to",
                "x-original-to",
                "envelope-to",
                "return-path",
            ]
            .iter()
            .any(|h| k.eq_ignore_ascii_case(h))
        })
        .map(|(_, v)| v)
        .chain(original_value("return-path"))
        .collect();
    Ok(FeedbackReport {
        kind,
        recipient,
        return_path: return_path_evidence(&outer),
        original_message_id: original_value("message-id").and_then(email_id_from_message_id),
    })
}

#[cfg(test)]
pub(crate) mod fixtures {
    pub const DSN: &str = include_str!("../tests/fixtures/dsn.eml");
    pub const ARF: &str = include_str!("../tests/fixtures/arf.eml");
    pub const PLAIN: &str = include_str!("../tests/fixtures/plain.eml");

    /// Fills `{{NAME}}` placeholders; unset ones become empty.
    pub fn render(template: &str, values: &[(&str, &str)]) -> String {
        let mut out = template.to_owned();
        for key in [
            "BOUNCE_TO",
            "RECIPIENT",
            "ACTION",
            "STATUS",
            "RETURN_PATH",
            "MESSAGE_ID",
            "ORIGINAL_RETURN_PATH",
        ] {
            let value = values
                .iter()
                .find(|(k, _)| *k == key)
                .map_or("", |(_, v)| *v);
            out = out.replace(&format!("{{{{{key}}}}}"), value);
        }
        out.replace('\n', "\r\n")
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::{ARF, DSN, PLAIN, render};
    use super::*;

    const ID: &str = "018f4e3c-2d1a-7b3c-8f00-1234567890ab";
    const VERP: &str =
        "bounces+018f4e3c-2d1a-7b3c-8f00-1234567890ab-t6hpvmqrhhcbxr7n@bounces.example.com";

    fn dsn(action: &str, status: &str, verp: &str) -> String {
        render(
            DSN,
            &[
                ("BOUNCE_TO", verp),
                ("RECIPIENT", "Alice@Example.org"),
                ("ACTION", action),
                ("STATUS", status),
                ("RETURN_PATH", verp),
                ("MESSAGE_ID", &format!("{ID}@example.com")),
            ],
        )
    }

    fn id() -> EmailId {
        EmailId::from(uuid::Uuid::parse_str(ID).unwrap())
    }

    #[test]
    fn hard_bounce_is_read_with_its_signed_return_path() {
        let report = parse_report(dsn("failed", "5.1.1", VERP).as_bytes()).unwrap();

        assert_eq!(report.recipient, "Alice@Example.org");
        assert_eq!(
            report.kind,
            ReportKind::Bounce {
                hard: true,
                diagnostic_code: Some(
                    "smtp; 550 5.1.1 <Alice@Example.org>: Recipient address rejected: User unknown"
                        .to_owned()
                ),
            }
        );
        assert_eq!(
            report.return_path,
            ReturnPathEvidence::Signed {
                id: id(),
                tag: "t6hpvmqrhhcbxr7n".to_owned()
            }
        );
        assert_eq!(report.original_message_id, Some(id()));
    }

    #[test]
    fn delayed_and_transient_statuses_are_soft_bounces() {
        for (action, status) in [("delayed", "4.4.1"), ("failed", "4.2.2")] {
            let report = parse_report(dsn(action, status, VERP).as_bytes()).unwrap();
            assert!(
                matches!(report.kind, ReportKind::Bounce { hard: false, .. }),
                "{action} {status}"
            );
        }
    }

    #[test]
    fn a_dsn_without_a_bounce_address_or_with_a_broken_one_is_flagged() {
        let none =
            parse_report(dsn("failed", "5.1.1", "postmaster@example.com").as_bytes()).unwrap();
        assert_eq!(none.return_path, ReturnPathEvidence::Absent);
        let broken = parse_report(
            dsn(
                "failed",
                "5.1.1",
                "bounces+not-a-uuid-xx@bounces.example.com",
            )
            .as_bytes(),
        )
        .unwrap();
        assert_eq!(broken.return_path, ReturnPathEvidence::Malformed);
        let short_tag = parse_report(
            dsn(
                "failed",
                "5.1.1",
                &format!("bounces+{ID}-abc@bounces.example.com"),
            )
            .as_bytes(),
        )
        .unwrap();
        assert_eq!(short_tag.return_path, ReturnPathEvidence::Malformed);
    }

    #[test]
    fn unsupported_dsn_actions_and_statuses_are_refused() {
        for (action, status) in [
            ("delivered", "2.0.0"),
            ("failed", "2.0.0"),
            ("failed", "garbage"),
        ] {
            assert!(
                parse_report(dsn(action, status, VERP).as_bytes()).is_err(),
                "{action} {status}"
            );
        }
    }

    #[test]
    fn abuse_report_uses_the_original_message_id_when_there_is_no_return_path() {
        let raw = render(
            ARF,
            &[
                ("BOUNCE_TO", "feedback@example.net"),
                ("RECIPIENT", "bob@example.org"),
                ("MESSAGE_ID", &format!("{ID}@example.com")),
            ],
        );

        let report = parse_report(raw.as_bytes()).unwrap();

        assert_eq!(report.kind, ReportKind::Complaint);
        assert_eq!(report.recipient, "bob@example.org");
        assert_eq!(report.return_path, ReturnPathEvidence::Absent);
        assert_eq!(report.original_message_id, Some(id()));
    }

    #[test]
    fn abuse_report_carries_the_signed_return_path_of_the_original() {
        let line = format!("Return-Path: <{VERP}>\n");
        let raw = render(
            ARF,
            &[
                ("RECIPIENT", "bob@example.org"),
                ("MESSAGE_ID", &format!("{ID}@example.com")),
                ("ORIGINAL_RETURN_PATH", &line),
            ],
        );

        let report = parse_report(raw.as_bytes()).unwrap();

        assert!(matches!(
            report.return_path,
            ReturnPathEvidence::Signed { .. }
        ));
    }

    #[test]
    fn other_messages_and_report_types_are_refused() {
        assert!(parse_report(PLAIN.as_bytes()).is_err());
        assert!(parse_report(b"").is_err());
        let other = render(ARF, &[("RECIPIENT", "bob@example.org")])
            .replace("Feedback-Type: abuse", "Feedback-Type: virus");
        assert!(parse_report(other.as_bytes()).is_err());
        let wrong_type = render(ARF, &[("RECIPIENT", "bob@example.org")]).replace(
            "report-type=feedback-report",
            "report-type=disposition-notification",
        );
        assert!(parse_report(wrong_type.as_bytes()).is_err());
    }
}
