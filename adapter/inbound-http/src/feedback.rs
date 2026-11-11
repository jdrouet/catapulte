//! Parses a forwarded DSN (RFC 3464) or ARF abuse report (RFC 5965) into a `FeedbackReport`.
//! Everything read here is untrusted; the use case decides what it is worth.

use catapulte_domain::entity::email::{EmailId, bare_address};
use catapulte_domain::use_case::ingest_feedback::{
    FeedbackReport, ReportItem, ReportKind, ReturnPathEvidence,
};
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

/// Most candidate return paths examined in one report; more is treated as hostile.
const MAX_CANDIDATES: usize = 16;
/// `bounces+{uuid}-{tag}`: a 36-character uuid, a dash and a 16-character tag.
const TOKEN_LEN: usize = 36 + 1 + 16;

/// Looks for `bounces+{uuid}-{tag}@` in the headers that can carry the return path, in one
/// pass over each header. Stops at the first malformed or conflicting candidate.
fn return_path_evidence(headers: &[&str]) -> ReturnPathEvidence {
    const MARKER: &str = "bounces+";
    let mut first: Option<ReturnPathEvidence> = None;
    let mut seen = 0;
    for value in headers {
        let lower = value.to_ascii_lowercase();
        let mut rest = lower.as_str();
        while let Some(at) = rest.find(MARKER) {
            seen += 1;
            if seen > MAX_CANDIDATES {
                return ReturnPathEvidence::Malformed;
            }
            let after = &rest[at + MARKER.len()..];
            // A token is bounded by its delimiter or, failing that, by one byte past the
            // expected length; this keeps the scan linear however many markers follow.
            let window = after
                .as_bytes()
                .get(..=TOKEN_LEN)
                .unwrap_or(after.as_bytes());
            let end = window
                .iter()
                .position(|b| matches!(b, b'@' | b'>' | b' ' | b',' | b';'))
                .unwrap_or(window.len());
            let candidate = after
                .get(..end)
                .map_or(ReturnPathEvidence::Malformed, parse_token);
            if candidate == ReturnPathEvidence::Malformed
                || first.as_ref().is_some_and(|f| *f != candidate)
            {
                return ReturnPathEvidence::Malformed;
            }
            first = Some(candidate);
            rest = &after[end..];
        }
    }
    first.unwrap_or(ReturnPathEvidence::Absent)
}

fn parse_token(token: &str) -> ReturnPathEvidence {
    if !token.is_ascii() || token.len() != TOKEN_LEN {
        return ReturnPathEvidence::Malformed;
    }
    let (id, tag) = token.split_at(36);
    let parsed = tag.strip_prefix('-').and_then(|tag| {
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

/// One per-recipient DSN block; `None` when it is not a failure or delay notification.
fn dsn_item(group: &[(String, String)]) -> Option<ReportItem> {
    // Original-Recipient is what Catapulte addressed; Final-Recipient may be after aliasing.
    let recipient = recipient_address(
        field(group, "original-recipient").or_else(|| field(group, "final-recipient"))?,
    );
    let action = field(group, "action")?.to_ascii_lowercase();
    let status = field(group, "status")?;
    let class = status.chars().next();
    let valid_status = status.split('.').count() == 3 && matches!(class, Some('4' | '5'));
    if recipient.is_empty() || !(action == "failed" || action == "delayed") || !valid_status {
        return None;
    }
    Some(ReportItem {
        kind: ReportKind::Bounce {
            hard: action == "failed" && class == Some('5'),
            diagnostic_code: field(group, "diagnostic-code").map(str::to_owned),
        },
        recipient,
    })
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

    let items = match report_type.as_str() {
        "delivery-status" => {
            let part = status_part("delivery-status").ok_or(NotAReport("no delivery status"))?;
            let items: Vec<ReportItem> = blocks(&part_text(part))
                .iter()
                .filter_map(|group| dsn_item(group))
                .collect();
            if items.is_empty() {
                return Err(NotAReport("no failure or delay notification"));
            }
            items
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
                .filter(|r| !r.is_empty())
                .ok_or(NotAReport("no reported recipient"))?;
            vec![ReportItem {
                kind: ReportKind::Complaint,
                recipient,
            }]
        }
        _ => return Err(NotAReport("unsupported report type")),
    };

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
        items,
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

        assert_eq!(report.items.len(), 1);
        assert_eq!(report.items[0].recipient, "Alice@Example.org");
        assert_eq!(
            report.items[0].kind,
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
                matches!(report.items[0].kind, ReportKind::Bounce { hard: false, .. }),
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

        assert_eq!(report.items[0].kind, ReportKind::Complaint);
        assert_eq!(report.items[0].recipient, "bob@example.org");
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

    #[test]
    fn a_multibyte_character_at_the_split_point_is_malformed_not_a_panic() {
        let token = format!("{}é{}", "a".repeat(35), "b".repeat(16));
        assert_eq!(token.len(), 53, "right length, with byte 36 inside the é");
        let header = format!("bounces+{token}@bounces.example.com");

        assert_eq!(
            return_path_evidence(&[&header]),
            ReturnPathEvidence::Malformed
        );
        let raw = dsn("failed", "5.1.1", &header);
        assert_eq!(
            parse_report(raw.as_bytes()).unwrap().return_path,
            ReturnPathEvidence::Malformed
        );
    }

    #[test]
    fn a_megabyte_of_markers_is_parsed_in_linear_time() {
        let hostile = "bounces+".repeat(131_072);
        assert_eq!(hostile.len(), 1024 * 1024);
        let started = std::time::Instant::now();

        let evidence = return_path_evidence(&[&hostile]);
        let raw = dsn("failed", "5.1.1", &hostile);
        let parsed = parse_report(raw.as_bytes());

        let elapsed = started.elapsed();
        eprintln!("hostile VERP scan took {elapsed:?}");
        assert!(elapsed < std::time::Duration::from_secs(2), "{elapsed:?}");
        assert_eq!(evidence, ReturnPathEvidence::Malformed);
        assert!(parsed.map_or(true, |r| r.return_path == ReturnPathEvidence::Malformed));
    }

    #[test]
    fn more_than_sixteen_candidates_or_conflicting_ones_are_malformed() {
        let valid = format!("bounces+{ID}-t6hpvmqrhhcbxr7n@b.example");
        let many = vec![valid.as_str(); 17].join(" ");
        assert_eq!(
            return_path_evidence(&[&many]),
            ReturnPathEvidence::Malformed
        );
        let sixteen = vec![valid.as_str(); 16].join(" ");
        assert!(matches!(
            return_path_evidence(&[&sixteen]),
            ReturnPathEvidence::Signed { .. }
        ));
        let other = format!("bounces+{ID}-aaaaaaaaaaaaaaaa@b.example");
        assert_eq!(
            return_path_evidence(&[&format!("{valid} {other}")]),
            ReturnPathEvidence::Malformed
        );
    }

    #[test]
    fn every_recipient_block_is_read_and_original_recipient_wins() {
        let raw = dsn("failed", "5.1.1", VERP).replace(
            "Final-Recipient: rfc822; Alice@Example.org\r\n",
            "Final-Recipient: rfc822; alias@remote.example\r\nOriginal-Recipient: rfc822; Alice@Example.org\r\n",
        );
        let second =
            "\r\nFinal-Recipient: rfc822; bob@example.org\r\nAction: delayed\r\nStatus: 4.2.2\r\n";
        let raw = raw.replace(
            "\r\n--BOUND\r\nContent-Type: message/rfc822",
            &format!("{second}\r\n--BOUND\r\nContent-Type: message/rfc822"),
        );

        let report = parse_report(raw.as_bytes()).unwrap();

        let recipients: Vec<_> = report.items.iter().map(|i| i.recipient.as_str()).collect();
        assert_eq!(recipients, ["Alice@Example.org", "bob@example.org"]);
        assert!(matches!(
            report.items[1].kind,
            ReportKind::Bounce { hard: false, .. }
        ));
    }
}
