use anyhow::Context;
use base64::Engine;
use catapulte_domain::entity::body::{BodySource, InvalidPlainBody, MjmlSource, Plain};
use catapulte_domain::entity::email::{EmailId, RecipientKind};
use catapulte_domain::entity::envelope::Unsubscribe;
use catapulte_domain::use_case::submit_email::{AttachmentInput, SubmitEmailInput};
use lettre::Address;
use lettre::message::Mailbox;
use lettre::message::header::ContentType;
use serde::ser::SerializeStruct;
use serde::{Deserialize, Serialize, Serializer};

pub const MAX_ATTACHMENTS_PER_EMAIL: usize = 10;
pub const MAX_ATTACHMENT_FILENAME_BYTES: usize = 255;
pub const MAX_ATTACHMENT_BYTES: u64 = 25 * 1024 * 1024; // 25 MiB
pub const MAX_ENVELOPE_BYTES: usize = 1024 * 1024; // 1 MiB
// Worst case: 10 attachments × 25 MiB = 250 MiB binary, base64-inflated to ~333 MiB, plus ~1 MiB envelope.
pub const MAX_REQUEST_BODY_BYTES: usize = 352 * 1024 * 1024; // 352 MiB
pub const MAX_EMAILS_PER_BATCH: usize = 100;
pub const MAX_HEADERS_PER_EMAIL: usize = 10;
pub const MAX_HEADER_VALUE_BYTES: usize = 998;
pub const MAX_REPLY_TO_PER_EMAIL: usize = 10;
pub const MAX_TAGS_PER_EMAIL: usize = 5;
pub const MAX_TAG_CHARS: usize = 64;
pub const MAX_METADATA_KEYS: usize = 10;
pub const MAX_METADATA_KEY_BYTES: usize = 64;
pub const MAX_METADATA_VALUE_BYTES: usize = 1024;
/// RFC 5322 limit on a physical header line, excluding CRLF.
const MAX_HEADER_LINE_CHARS: usize = 998;

#[derive(Debug, Deserialize)]
pub struct BatchSubmitEmailRequest {
    pub emails: Vec<SubmitEmailRequest>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum BatchItemResultDto {
    Accepted { id: String },
    Rejected { error: String },
}

#[derive(Debug, Serialize)]
pub struct BatchSubmitEmailResponse {
    pub results: Vec<BatchItemResultDto>,
}

#[derive(Debug, Deserialize)]
pub struct AttachmentDto {
    pub filename: String,
    pub content_type: String,
    #[serde(default)]
    pub inline_base64: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    /// Makes the attachment inline, referenced from HTML as `cid:<content_id>`.
    #[serde(default)]
    pub content_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SubmitEmailRequest {
    #[serde(flatten)]
    pub core: EnvelopeCoreDto,
    #[serde(default)]
    pub attachments: Vec<AttachmentDto>,
}

/// A mailbox given either as text (`a@b.c` or `Name <a@b.c>`) or as `{ name, address }`.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum MailboxDto {
    Text(String),
    Structured {
        #[serde(default)]
        name: Option<String>,
        address: String,
    },
}

/// One mailbox or a list of mailboxes.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum ReplyToDto {
    Many(Vec<MailboxDto>),
    One(MailboxDto),
}

#[derive(Debug, Deserialize)]
pub struct RecipientDto {
    pub kind: RecipientKindDto,
    pub address: String,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecipientKindDto {
    To,
    Cc,
    Bcc,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BodyDto {
    Plain {
        text: Option<String>,
        html: Option<String>,
    },
    MjmlInline {
        source: String,
    },
    MjmlNamed {
        name: String,
    },
    MjmlRemote {
        url: String,
    },
}

pub struct SubmitEmailResponse {
    pub id: EmailId,
}

impl Serialize for SubmitEmailResponse {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut s = serializer.serialize_struct("SubmitEmailResponse", 1)?;
        s.serialize_field("id", &self.id.as_uuid().to_string())?;
        s.end()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BodyConversionError {
    #[error("invalid plain body")]
    InvalidPlain(#[source] InvalidPlainBody),
    #[error("invalid remote url")]
    InvalidRemoteUrl(#[source] anyhow::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum EnvelopeConversionError {
    #[error(transparent)]
    Body(#[from] BodyConversionError),
    #[error("invalid sender address")]
    InvalidSender(#[source] anyhow::Error),
    #[error("subject line is too long")]
    InvalidSubject,
    #[error("invalid reply_to address")]
    InvalidReplyTo(#[source] anyhow::Error),
    #[error("invalid header: {0}")]
    InvalidHeader(&'static str),
    #[error("invalid unsubscribe: {0}")]
    InvalidUnsubscribe(&'static str),
    #[error("invalid tags: {0}")]
    InvalidTags(&'static str),
    #[error("invalid metadata: {0}")]
    InvalidMetadata(&'static str),
    #[error("recipients must not be empty")]
    EmptyRecipients,
    #[error("invalid recipient address")]
    InvalidRecipient(#[source] anyhow::Error),
    #[error("invalid attachment base64")]
    InvalidAttachmentBase64 {
        filename: String,
        #[source]
        source: anyhow::Error,
    },
    #[error("invalid attachment shape: exactly one of inline_base64 or url must be set")]
    InvalidAttachmentShape { filename: String },
    #[error("invalid attachment url")]
    InvalidAttachmentUrl {
        filename: String,
        #[source]
        source: anyhow::Error,
    },
    #[error("invalid attachment content type")]
    InvalidAttachmentContentType { filename: String },
    #[error("invalid attachment filename")]
    InvalidAttachmentFilename { filename: String },
    #[error("invalid attachment content_id: {0}")]
    InvalidContentId(&'static str),
    #[error("duplicate attachment content_id")]
    DuplicateContentId,
    #[error("too many attachments")]
    TooManyAttachments,
    #[error("attachment too large")]
    AttachmentTooLarge { filename: String },
}

/// Must stay identical to the SMTP adapter's `parse_mailbox`, so intake accepts exactly
/// what delivery can parse.
fn parse_mailbox(s: &str) -> Result<Mailbox, anyhow::Error> {
    s.parse::<Mailbox>().or_else(|_| {
        s.parse::<Address>()
            .map(|a| Mailbox::new(None, a))
            .with_context(|| format!("invalid mailbox: {s}"))
    })
}

fn format_mailbox(mailbox: &Mailbox) -> Result<String, anyhow::Error> {
    use std::fmt::Write as _;
    let mut out = String::new();
    write!(out, "{mailbox}").context("formatting mailbox")?;
    Ok(out)
}

/// Formats `mailbox` and accepts it only if delivery can parse the result back to the same
/// string and lettre can build message headers from it.
fn canonical_mailbox(mailbox: &Mailbox) -> Result<String, anyhow::Error> {
    anyhow::ensure!(
        !mailbox
            .name
            .as_deref()
            .is_some_and(|n| n.chars().any(char::is_control)),
        "display name contains control characters"
    );
    let formatted = format_mailbox(mailbox)?;
    let reparsed = format_mailbox(&parse_mailbox(&formatted)?)?;
    anyhow::ensure!(
        reparsed == formatted,
        "mailbox does not round-trip: {formatted}"
    );
    // lettre cannot build headers for some valid addresses (IP literals, quoted local parts).
    lettre::Message::builder()
        .from(mailbox.clone())
        .to(mailbox.clone())
        .body(String::new())
        .context("mailbox cannot be used in message headers")?;
    Ok(formatted)
}

fn structured_mailbox(name: Option<String>, address: &str) -> Result<Mailbox, anyhow::Error> {
    let address = address.parse::<Address>().context("parsing address")?;
    Ok(Mailbox::new(name.filter(|n| !n.is_empty()), address))
}

fn validate_mailbox(mailbox: MailboxDto) -> Result<String, anyhow::Error> {
    match mailbox {
        MailboxDto::Text(text) => parse_mailbox(&text),
        MailboxDto::Structured { name, address } => structured_mailbox(name, &address),
    }
    .and_then(|m| canonical_mailbox(&m))
}

fn validate_sender(sender: MailboxDto) -> Result<String, EnvelopeConversionError> {
    validate_mailbox(sender).map_err(EnvelopeConversionError::InvalidSender)
}

fn validate_reply_to(reply_to: Option<ReplyToDto>) -> Result<Vec<String>, EnvelopeConversionError> {
    let mailboxes = match reply_to {
        None => return Ok(vec![]),
        Some(ReplyToDto::One(m)) => vec![m],
        Some(ReplyToDto::Many(ms)) => ms,
    };
    if mailboxes.len() > MAX_REPLY_TO_PER_EMAIL {
        return Err(EnvelopeConversionError::InvalidReplyTo(anyhow::anyhow!(
            "too many reply_to addresses"
        )));
    }
    mailboxes
        .into_iter()
        .map(|m| validate_mailbox(m).map_err(EnvelopeConversionError::InvalidReplyTo))
        .collect()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnsubscribeDto {
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub mailto: Option<String>,
}

fn validate_unsubscribe(
    dto: Option<UnsubscribeDto>,
) -> Result<Option<Unsubscribe>, EnvelopeConversionError> {
    use EnvelopeConversionError::InvalidUnsubscribe;
    let Some(dto) = dto else {
        return Ok(None);
    };
    if dto.url.is_none() && dto.mailto.is_none() {
        return Err(InvalidUnsubscribe("set at least one of url or mailto"));
    }
    let url = dto
        .url
        .map(|raw| {
            url::Url::parse(&raw)
                .ok()
                .filter(|u| u.scheme() == "https")
                .ok_or(InvalidUnsubscribe("url must be a valid https URL"))
        })
        .transpose()?;
    let mailto = dto
        .mailto
        .map(|raw| {
            raw.parse::<Address>()
                .ok()
                .and_then(|a| Unsubscribe::normalize_mailto(a.as_ref()))
                .ok_or(InvalidUnsubscribe(
                    "mailto must be an address with only letters, digits and . _ + - in the local part",
                ))
        })
        .transpose()?;
    Ok(Some(Unsubscribe { url, mailto }))
}

/// Builds the message headers exactly as the SMTP adapter does (same lettre builder calls
/// in the same order: from, recipients, reply-to, custom headers, subject) and rejects any
/// physical header line over 998 characters. lettre folds only at whitespace, so a long
/// unbroken value stays one line. The adapter repeats the check on the final bytes.
fn check_header_lines(
    sender: &str,
    recipients: &[(RecipientKind, String)],
    subject: Option<&str>,
    reply_to: &[String],
    headers: &[(String, String)],
    unsubscribe: Option<&Unsubscribe>,
) -> Result<(), EnvelopeConversionError> {
    use EnvelopeConversionError::InvalidHeader;
    use lettre::message::header::{HeaderName, HeaderValue};
    let unbuildable = || InvalidHeader("cannot build message headers");
    let mut builder =
        lettre::Message::builder().from(parse_mailbox(sender).map_err(|_| unbuildable())?);
    for (kind, address) in recipients {
        let mailbox = parse_mailbox(address).map_err(|_| unbuildable())?;
        builder = match kind {
            RecipientKind::To => builder.to(mailbox),
            RecipientKind::Cc => builder.cc(mailbox),
            RecipientKind::Bcc => builder.bcc(mailbox),
        };
    }
    for address in reply_to {
        builder = builder.reply_to(parse_mailbox(address).map_err(|_| unbuildable())?);
    }
    for (name, value) in headers {
        let name = HeaderName::new_from_ascii(name.clone()).map_err(|_| unbuildable())?;
        builder = builder.raw_header(HeaderValue::new(name, value.clone()));
    }
    for (name, value) in unsubscribe.map(Unsubscribe::headers).unwrap_or_default() {
        let name = HeaderName::new_from_ascii(name.to_owned()).map_err(|_| unbuildable())?;
        builder = builder.raw_header(HeaderValue::new(name, value));
    }
    if let Some(subject) = subject {
        builder = builder.subject(subject);
    }
    let raw = builder
        .body(String::new())
        .map_err(|_| unbuildable())?
        .formatted();

    let mut current = String::new();
    for line in raw.split(|b| *b == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() {
            break;
        }
        if !matches!(line[0], b' ' | b'\t') {
            current = String::from_utf8_lossy(line.split(|b| *b == b':').next().unwrap_or(line))
                .to_ascii_lowercase();
        }
        if line.len() > MAX_HEADER_LINE_CHARS {
            return Err(match current.as_str() {
                "from" => EnvelopeConversionError::InvalidSender(anyhow::anyhow!(
                    "a header line exceeds {MAX_HEADER_LINE_CHARS} characters"
                )),
                "to" | "cc" | "bcc" => EnvelopeConversionError::InvalidRecipient(anyhow::anyhow!(
                    "a header line exceeds {MAX_HEADER_LINE_CHARS} characters"
                )),
                "reply-to" => EnvelopeConversionError::InvalidReplyTo(anyhow::anyhow!(
                    "a header line exceeds {MAX_HEADER_LINE_CHARS} characters"
                )),
                "subject" => EnvelopeConversionError::InvalidSubject,
                _ => InvalidHeader("header line too long"),
            });
        }
    }
    Ok(())
}

/// Headers Catapulte or the transport owns; compared case-insensitively.
const RESERVED_HEADERS: &[&str] = &[
    "from",
    "to",
    "cc",
    "bcc",
    "subject",
    "date",
    "message-id",
    "mime-version",
    "return-path",
    "dkim-signature",
    "reply-to",
    "sender",
    "received",
    "authentication-results",
    "return-receipt-to",
    "disposition-notification-to",
];

fn is_reserved_header(lower: &str) -> bool {
    RESERVED_HEADERS.contains(&lower)
        || lower.starts_with("content-")
        || lower.starts_with("list-unsubscribe")
        || lower.starts_with("resent-")
}

fn validate_headers(
    headers: std::collections::BTreeMap<String, String>,
) -> Result<Vec<(String, String)>, EnvelopeConversionError> {
    use EnvelopeConversionError::InvalidHeader;
    if headers.len() > MAX_HEADERS_PER_EMAIL {
        return Err(InvalidHeader("too many headers"));
    }
    let mut seen = std::collections::HashSet::new();
    for (name, value) in &headers {
        if !name.bytes().all(|b| (33..=126).contains(&b) && b != b':')
            || lettre::message::header::HeaderName::new_from_ascii(name.clone()).is_err()
        {
            return Err(InvalidHeader("invalid header name"));
        }
        let lower = name.to_ascii_lowercase();
        if is_reserved_header(&lower) {
            return Err(InvalidHeader("reserved header name"));
        }
        if !seen.insert(lower) {
            return Err(InvalidHeader("duplicate header name"));
        }
        if value.len() > MAX_HEADER_VALUE_BYTES {
            return Err(InvalidHeader("header value too long"));
        }
        if value.chars().any(|c| c.is_control() && c != '\t') {
            return Err(InvalidHeader("header value contains control characters"));
        }
    }
    let headers: Vec<(String, String)> = headers.into_iter().collect();
    Ok(headers)
}

fn validate_recipients(
    recipients: Vec<RecipientDto>,
) -> Result<Vec<(catapulte_domain::entity::email::RecipientKind, String)>, EnvelopeConversionError>
{
    if recipients.is_empty() {
        return Err(EnvelopeConversionError::EmptyRecipients);
    }
    recipients
        .into_iter()
        .map(|r| {
            let canonical = match r.name {
                Some(name) => structured_mailbox(Some(name), &r.address),
                None => parse_mailbox(&r.address),
            }
            .and_then(|m| canonical_mailbox(&m))
            .map_err(EnvelopeConversionError::InvalidRecipient)?;
            Ok((r.kind.into(), canonical))
        })
        .collect::<Result<Vec<_>, EnvelopeConversionError>>()
}

/// Bidi and zero-width format characters (Unicode category Cf), rejected to prevent
/// filename spoofing such as `invoice\u{202E}fdp.exe`.
fn is_format_char(c: char) -> bool {
    matches!(
        c,
        '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{FEFF}'
    )
}

/// Returns the filename's last path segment and the content type unchanged, so the SMTP
/// adapter's `ContentType::parse` sees exactly what was validated here.
///
/// # Errors
///
/// Returns an error when the content type does not parse, or the filename is empty after
/// stripping directories, longer than 255 bytes, or contains control characters.
pub fn validate_attachment_meta(
    filename: &str,
    content_type: &str,
) -> Result<(String, String), EnvelopeConversionError> {
    let name = filename
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default()
        .trim();
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.len() > MAX_ATTACHMENT_FILENAME_BYTES
        || name.chars().any(|c| c.is_control() || is_format_char(c))
    {
        return Err(EnvelopeConversionError::InvalidAttachmentFilename {
            filename: filename.to_owned(),
        });
    }
    ContentType::parse(content_type).map_err(|_| {
        EnvelopeConversionError::InvalidAttachmentContentType {
            filename: filename.to_owned(),
        }
    })?;
    Ok((name.to_owned(), content_type.to_owned()))
}

const MAX_CONTENT_ID_BYTES: usize = 255;

/// # Errors
///
/// Returns `InvalidContentId` when the value is empty, longer than 255 bytes, or contains
/// anything other than ASCII graphic characters (no whitespace, `<` or `>`).
pub fn validate_content_id(
    content_id: Option<String>,
) -> Result<Option<String>, EnvelopeConversionError> {
    let Some(cid) = content_id else {
        return Ok(None);
    };
    if cid.is_empty() {
        return Err(EnvelopeConversionError::InvalidContentId(
            "must not be empty",
        ));
    }
    if cid.len() > MAX_CONTENT_ID_BYTES {
        return Err(EnvelopeConversionError::InvalidContentId(
            "must be at most 255 bytes",
        ));
    }
    if !cid
        .chars()
        .all(|c| c.is_ascii_graphic() && c != '<' && c != '>')
    {
        return Err(EnvelopeConversionError::InvalidContentId(
            "must contain only ASCII graphic characters, without '<' or '>'",
        ));
    }
    Ok(Some(cid))
}

fn attachment_dto_to_input(a: AttachmentDto) -> Result<AttachmentInput, EnvelopeConversionError> {
    let (filename, content_type) = validate_attachment_meta(&a.filename, &a.content_type)?;
    let content_id = validate_content_id(a.content_id.clone())?;
    let a = AttachmentDto {
        filename,
        content_type,
        ..a
    };
    match (a.inline_base64, a.url) {
        (Some(b64), None) => {
            // Reject before decoding so an oversized payload is never allocated.
            if b64.len() as u64 > MAX_ATTACHMENT_BYTES.div_ceil(3) * 4 + 4 {
                return Err(EnvelopeConversionError::AttachmentTooLarge {
                    filename: a.filename,
                });
            }
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(&b64)
                .context(format!("decoding base64 for attachment '{}'", a.filename))
                .map_err(|source| EnvelopeConversionError::InvalidAttachmentBase64 {
                    filename: a.filename.clone(),
                    source,
                })?;
            if decoded.len() as u64 > MAX_ATTACHMENT_BYTES {
                return Err(EnvelopeConversionError::AttachmentTooLarge {
                    filename: a.filename,
                });
            }
            Ok(AttachmentInput::Inline {
                filename: a.filename,
                content_type: a.content_type,
                bytes: bytes::Bytes::from(decoded),
                content_id,
            })
        }
        (None, Some(raw_url)) => {
            let parsed = url::Url::parse(&raw_url)
                .context(format!("parsing url for attachment '{}'", a.filename))
                .map_err(|source| EnvelopeConversionError::InvalidAttachmentUrl {
                    filename: a.filename.clone(),
                    source,
                })?;
            Ok(AttachmentInput::Remote {
                filename: a.filename,
                content_type: a.content_type,
                url: parsed,
                content_id,
            })
        }
        _ => Err(EnvelopeConversionError::InvalidAttachmentShape {
            filename: a.filename,
        }),
    }
}

impl SubmitEmailRequest {
    /// # Errors
    ///
    /// Returns an error when the request contains invalid fields (bad sender,
    /// recipients, body, or attachments).
    pub fn into_submit_input(self) -> Result<SubmitEmailInput, EnvelopeConversionError> {
        if self.attachments.len() > MAX_ATTACHMENTS_PER_EMAIL {
            return Err(EnvelopeConversionError::TooManyAttachments);
        }
        let mut atts = Vec::with_capacity(self.attachments.len());
        for a in self.attachments {
            atts.push(attachment_dto_to_input(a)?);
        }
        self.core.into_submit_input(atts)
    }
}

/// Envelope fields shared by JSON, multipart, batch and NATS submissions.
#[derive(Debug, Deserialize)]
pub struct EnvelopeCoreDto {
    #[serde(default)]
    pub idempotency_key: Option<String>,
    #[serde(default)]
    pub correlation_id: Option<String>,
    #[serde(default)]
    pub subject: Option<String>,
    pub sender: MailboxDto,
    pub recipients: Vec<RecipientDto>,
    pub body: BodyDto,
    #[serde(default)]
    pub variables: serde_json::Map<String, serde_json::Value>,
    #[serde(default)]
    pub reply_to: Option<ReplyToDto>,
    #[serde(default, deserialize_with = "deserialize_headers")]
    pub headers: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub unsubscribe: Option<UnsubscribeDto>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub metadata: std::collections::BTreeMap<String, String>,
}

fn validate_tags(tags: &[String]) -> Result<(), EnvelopeConversionError> {
    use EnvelopeConversionError::InvalidTags;
    if tags.len() > MAX_TAGS_PER_EMAIL {
        return Err(InvalidTags("too many tags"));
    }
    for tag in tags {
        let valid = (1..=MAX_TAG_CHARS).contains(&tag.len())
            && tag.bytes().all(|b| {
                b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'.' | b'-')
            });
        if !valid {
            return Err(InvalidTags(
                "each tag is 1-64 characters from a-z, 0-9, _, . and -",
            ));
        }
    }
    Ok(())
}

fn validate_metadata(
    metadata: &std::collections::BTreeMap<String, String>,
) -> Result<(), EnvelopeConversionError> {
    use EnvelopeConversionError::InvalidMetadata;
    if metadata.len() > MAX_METADATA_KEYS {
        return Err(InvalidMetadata("too many keys"));
    }
    for (key, value) in metadata {
        if key.is_empty() || key.len() > MAX_METADATA_KEY_BYTES {
            return Err(InvalidMetadata("keys are 1-64 bytes"));
        }
        if value.len() > MAX_METADATA_VALUE_BYTES {
            return Err(InvalidMetadata("value too long"));
        }
        if key.chars().chain(value.chars()).any(char::is_control) {
            return Err(InvalidMetadata("control characters are not allowed"));
        }
    }
    Ok(())
}

/// Rejects a JSON object that repeats a header name, which a plain map would silently
/// resolve to the last value.
fn deserialize_headers<'de, D>(
    deserializer: D,
) -> Result<std::collections::BTreeMap<String, String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct Visitor;

    impl<'de> serde::de::Visitor<'de> for Visitor {
        type Value = std::collections::BTreeMap<String, String>;

        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("an object of header names to string values")
        }

        fn visit_map<A: serde::de::MapAccess<'de>>(
            self,
            mut map: A,
        ) -> Result<Self::Value, A::Error> {
            let mut out = std::collections::BTreeMap::new();
            while let Some((name, value)) = map.next_entry::<String, String>()? {
                if out.insert(name, value).is_some() {
                    return Err(serde::de::Error::custom("duplicate header name"));
                }
            }
            Ok(out)
        }
    }

    deserializer.deserialize_map(Visitor)
}

impl EnvelopeCoreDto {
    /// Convert envelope metadata and pre-built stream attachments into a `SubmitEmailInput`.
    ///
    /// # Errors
    ///
    /// Returns `EnvelopeConversionError` when sender, recipients, or body fail validation.
    pub fn into_submit_input(
        self,
        attachments: Vec<AttachmentInput>,
    ) -> Result<SubmitEmailInput, EnvelopeConversionError> {
        let sender = validate_sender(self.sender)?;
        let recipients = validate_recipients(self.recipients)?;
        let reply_to = validate_reply_to(self.reply_to)?;
        let headers = validate_headers(self.headers)?;
        let unsubscribe = validate_unsubscribe(self.unsubscribe)?;
        validate_tags(&self.tags)?;
        validate_metadata(&self.metadata)?;
        check_header_lines(
            &sender,
            &recipients,
            self.subject.as_deref(),
            &reply_to,
            &headers,
            unsubscribe.as_ref(),
        )?;
        let body = self.body.try_into()?;
        let mut seen = std::collections::HashSet::new();
        for att in &attachments {
            let (AttachmentInput::Inline { content_id, .. }
            | AttachmentInput::Stream { content_id, .. }
            | AttachmentInput::Remote { content_id, .. }) = att;
            if let Some(cid) = content_id
                && !seen.insert(cid.as_str())
            {
                return Err(EnvelopeConversionError::DuplicateContentId);
            }
        }
        Ok(SubmitEmailInput {
            owner: String::new(),
            sender_domains: None,
            idempotency_key: self.idempotency_key,
            correlation_id: self.correlation_id,
            subject: self.subject,
            sender,
            recipients,
            body,
            variables: self.variables,
            attachments,
            reply_to,
            headers,
            unsubscribe,
            tags: self.tags,
            metadata: self.metadata,
        })
    }
}

impl From<RecipientKindDto> for RecipientKind {
    fn from(k: RecipientKindDto) -> Self {
        match k {
            RecipientKindDto::To => Self::To,
            RecipientKindDto::Cc => Self::Cc,
            RecipientKindDto::Bcc => Self::Bcc,
        }
    }
}

fn plain_body(
    text: Option<String>,
    html: Option<String>,
) -> Result<BodySource, BodyConversionError> {
    Plain::try_new(text, html)
        .map(BodySource::Plain)
        .map_err(BodyConversionError::InvalidPlain)
}

fn remote_mjml_body(url: &str) -> Result<BodySource, BodyConversionError> {
    url::Url::parse(url)
        .context("parsing mjml remote url")
        .map(|parsed| BodySource::Mjml(MjmlSource::Remote(parsed)))
        .map_err(BodyConversionError::InvalidRemoteUrl)
}

impl TryFrom<BodyDto> for BodySource {
    type Error = BodyConversionError;

    fn try_from(b: BodyDto) -> Result<Self, Self::Error> {
        match b {
            BodyDto::Plain { text, html } => plain_body(text, html),
            BodyDto::MjmlInline { source } => Ok(Self::Mjml(MjmlSource::Inline(source))),
            BodyDto::MjmlNamed { name } => Ok(Self::Mjml(MjmlSource::Named(name))),
            BodyDto::MjmlRemote { url } => remote_mjml_body(&url),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AttachmentDto, BodyConversionError, BodyDto, BodySource, EnvelopeConversionError,
        EnvelopeCoreDto, MailboxDto, MjmlSource, RecipientDto, RecipientKindDto,
        SubmitEmailRequest,
    };

    fn base_core() -> EnvelopeCoreDto {
        EnvelopeCoreDto {
            idempotency_key: None,
            correlation_id: None,
            subject: None,
            sender: MailboxDto::Text("a@b.c".into()),
            recipients: vec![RecipientDto {
                kind: RecipientKindDto::To,
                address: "t@x.y".into(),
                name: None,
            }],
            body: BodyDto::Plain {
                text: Some("hi".into()),
                html: None,
            },
            variables: serde_json::Map::new(),
            reply_to: None,
            unsubscribe: None,
            tags: vec![],
            metadata: std::collections::BTreeMap::new(),
            headers: std::collections::BTreeMap::new(),
        }
    }

    fn base_request() -> SubmitEmailRequest {
        SubmitEmailRequest {
            core: base_core(),
            attachments: vec![],
        }
    }

    fn convert_with(
        extra: serde_json::Value,
    ) -> Result<catapulte_domain::use_case::submit_email::SubmitEmailInput, EnvelopeConversionError>
    {
        let mut env = serde_json::json!({
            "sender": "a@b.c",
            "recipients": [{"kind": "to", "address": "t@x.y"}],
            "body": {"kind": "plain", "text": "hi"}
        });
        let serde_json::Value::Object(extra) = extra else {
            panic!("extra must be an object");
        };
        for (k, v) in extra {
            env[k] = v;
        }
        serde_json::from_value::<SubmitEmailRequest>(env)
            .unwrap()
            .into_submit_input()
    }

    #[test]
    fn request_conversion_never_sets_the_caller_identity() {
        let input = convert_with(serde_json::json!({
            "owner": "x", "admin": true, "api_key": "k", "sender_domains": ["a.com"]
        }))
        .unwrap();
        assert_eq!(input.owner, "", "legacy key and NATS share this namespace");
        assert_eq!(input.sender_domains, None);
    }

    #[test]
    fn tags_and_metadata_are_validated() {
        let ok = convert_with(serde_json::json!({
            "tags": ["a", "b.c_d-1"], "metadata": { "order": "42" }
        }))
        .unwrap();
        assert_eq!(ok.tags, vec!["a".to_owned(), "b.c_d-1".to_owned()]);
        assert_eq!(ok.metadata["order"], "42");

        let long_tag = "a".repeat(65);
        let long_key = "k".repeat(65);
        let long_value = "v".repeat(1025);
        let many_keys: serde_json::Map<String, serde_json::Value> =
            (0..11).map(|i| (format!("k{i}"), "v".into())).collect();
        for bad in [
            serde_json::json!({ "tags": ["a", "b", "c", "d", "e", "f"] }),
            serde_json::json!({ "tags": [""] }),
            serde_json::json!({ "tags": ["Upper"] }),
            serde_json::json!({ "tags": ["has space"] }),
            serde_json::json!({ "tags": [long_tag] }),
            serde_json::json!({ "metadata": many_keys }),
            serde_json::json!({ "metadata": { "": "v" } }),
            serde_json::json!({ "metadata": { long_key: "v" } }),
            serde_json::json!({ "metadata": { "k": long_value } }),
            serde_json::json!({ "metadata": { "k": "a\u{0}b" } }),
            serde_json::json!({ "metadata": { "a\u{0}b": "v" } }),
            serde_json::json!({ "metadata": { "k": "line\nbreak" } }),
        ] {
            assert!(convert_with(bad.clone()).is_err(), "{bad}");
        }
        let edge = serde_json::json!({
            "tags": ["a", "b", "c", "d", "e"],
            "metadata": { "k".repeat(64): "v".repeat(1024) }
        });
        assert!(convert_with(edge).is_ok());
    }

    #[test]
    fn reply_to_accepts_one_or_many_and_canonicalises() {
        let one = convert_with(serde_json::json!({"reply_to": "Help <h@x.y>"})).unwrap();
        assert_eq!(one.reply_to, vec!["Help <h@x.y>".to_owned()]);
        let many = convert_with(serde_json::json!({
            "reply_to": ["a@x.y", {"name": "B", "address": "b@x.y"}]
        }))
        .unwrap();
        assert_eq!(
            many.reply_to,
            vec!["a@x.y".to_owned(), "B <b@x.y>".to_owned()]
        );
    }

    #[test]
    fn reply_to_with_control_characters_in_the_name_is_rejected() {
        let err = convert_with(serde_json::json!({
            "reply_to": {"name": "A\r\nB", "address": "a@x.y"}
        }))
        .err()
        .expect("must be rejected");
        assert!(matches!(err, EnvelopeConversionError::InvalidReplyTo(_)));
    }

    #[test]
    fn headers_are_kept_in_name_order() {
        let input =
            convert_with(serde_json::json!({"headers": {"X-B": "2", "X-A": "1\t2"}})).unwrap();
        assert_eq!(
            input.headers,
            vec![("X-A".into(), "1\t2".into()), ("X-B".into(), "2".into())]
        );
    }

    #[test]
    fn header_rules_reject_bad_names_values_and_counts() {
        let long_value = "v".repeat(999);
        let cases = [
            serde_json::json!({"headers": {"X-A": long_value}}),
            serde_json::json!({"headers": {"X-A": "a\nb"}}),
            serde_json::json!({"headers": {"X-A": "a\rb"}}),
            serde_json::json!({"headers": {"X-A": "a\u{7f}b"}}),
            serde_json::json!({"headers": {"X A": "v"}}),
            serde_json::json!({"headers": {"X-\u{e9}": "v"}}),
            serde_json::json!({"headers": {"": "v"}}),
            serde_json::json!({"headers": {"X".repeat(77): "v"}}),
            serde_json::json!({"headers": {"X-A": "1", "x-a": "2"}}),
        ];
        for case in cases {
            let err = convert_with(case.clone()).err().expect("must be rejected");
            assert!(
                matches!(err, EnvelopeConversionError::InvalidHeader(_)),
                "{case}"
            );
        }
        assert!(convert_with(serde_json::json!({"headers": {"X".repeat(76): "v"}})).is_ok());
    }

    #[test]
    fn header_line_limit_is_998_characters_including_the_name() {
        // "X-A: " is 5 characters, so 993 unbroken characters fill the line exactly.
        let value = |n: usize| serde_json::json!({"headers": {"X-A": "v".repeat(n)}});
        assert!(convert_with(value(993)).is_ok());
        assert!(convert_with(value(994)).is_err());
        let long_name = "N".repeat(76);
        // name + ": " is 78 characters.
        let named = |n: usize| serde_json::json!({"headers": {long_name.clone(): "v".repeat(n)}});
        assert!(convert_with(named(920)).is_ok());
        assert!(convert_with(named(921)).is_err());
    }

    #[test]
    fn long_value_with_spaces_is_folded_and_accepted_but_tabs_are_not_fold_points() {
        // 76 + 2 + 949 characters would be 1027 on one line, so acceptance proves folding.
        let spaced = "word ".repeat(190).trim_end().to_owned();
        assert!(spaced.len() <= super::MAX_HEADER_VALUE_BYTES);
        assert!(convert_with(serde_json::json!({"headers": {"N".repeat(76): spaced}})).is_ok());
        // lettre does not fold at HTAB, so the same shape with tabs stays one long line.
        let tabbed = "word\t".repeat(190);
        assert!(convert_with(serde_json::json!({"headers": {"N".repeat(76): tabbed}})).is_err());
    }

    #[test]
    fn over_long_subject_sender_and_recipient_names_are_rejected_under_the_limit_accepted() {
        let long = "n".repeat(1000);
        let under = "n".repeat(900);
        let subject = |s: &str| serde_json::json!({"subject": s});
        let sender = |n: &str| serde_json::json!({"sender": {"name": n, "address": "a@b.c"}});
        let recipient = |n: &str| serde_json::json!({"recipients": [{"kind": "to", "name": n, "address": "t@x.y"}]});
        assert!(matches!(
            convert_with(subject(&long))
                .err()
                .expect("must be rejected"),
            EnvelopeConversionError::InvalidSubject
        ));
        assert!(matches!(
            convert_with(sender(&long)).err().expect("must be rejected"),
            EnvelopeConversionError::InvalidSender(_)
        ));
        assert!(matches!(
            convert_with(recipient(&long))
                .err()
                .expect("must be rejected"),
            EnvelopeConversionError::InvalidRecipient(_)
        ));
        assert!(convert_with(subject(&under)).is_ok());
        assert!(convert_with(sender(&under)).is_ok());
        assert!(convert_with(recipient(&under)).is_ok());
    }

    #[test]
    fn unsubscribe_is_validated() {
        let ok = convert_with(serde_json::json!({
            "unsubscribe": {"url": "https://example.com/u/1", "mailto": "u@example.com"}
        }))
        .unwrap()
        .unsubscribe
        .unwrap();
        assert_eq!(ok.url.unwrap().as_str(), "https://example.com/u/1");
        assert_eq!(ok.mailto.as_deref(), Some("u@example.com"));
        assert!(
            convert_with(serde_json::json!({"unsubscribe": {"mailto": "u@example.com"}})).is_ok()
        );

        let long_url = format!("https://example.com/{}", "a".repeat(1100));
        let bad = [
            serde_json::json!({}),
            serde_json::json!({"url": "http://example.com/u"}),
            serde_json::json!({"url": "not a url"}),
            serde_json::json!({"mailto": "not an address"}),
            serde_json::json!({"url": long_url}),
        ];
        for case in bad {
            let err = convert_with(serde_json::json!({"unsubscribe": case.clone()}))
                .err()
                .expect("must be rejected");
            assert!(
                matches!(
                    err,
                    EnvelopeConversionError::InvalidUnsubscribe(_)
                        | EnvelopeConversionError::InvalidHeader(_)
                ),
                "{case}"
            );
        }
    }

    #[test]
    fn unsubscribe_mailto_charset_is_enforced_and_idn_is_punycoded() {
        let mailto = |m: &str| convert_with(serde_json::json!({"unsubscribe": {"mailto": m}}));
        for bad in [
            "\"x>, <https://evil\"@e.com",
            "\"a b\"@e.com",
            "a?subject=x@e.com",
            "a%41@e.com",
            "a&b@e.com",
            "a,b@e.com",
        ] {
            assert!(
                matches!(
                    mailto(bad).err().expect("must be rejected"),
                    EnvelopeConversionError::InvalidUnsubscribe(_)
                ),
                "{bad}"
            );
        }
        for good in ["unsubscribe+list-1@example.com", "a.b_c-d@example.com"] {
            let got = mailto(good).unwrap().unsubscribe.unwrap();
            assert_eq!(got.mailto.as_deref(), Some(good));
        }
        let idn = mailto("u@bücher.de").unwrap().unsubscribe.unwrap();
        assert_eq!(idn.mailto.as_deref(), Some("u@xn--bcher-kva.de"));
        assert_eq!(idn.headers()[0].1, "<mailto:u@xn--bcher-kva.de>");
    }

    #[test]
    fn over_long_reply_to_header_line_is_rejected() {
        let name = "n".repeat(1200);
        let result = convert_with(serde_json::json!({
            "reply_to": {"name": name, "address": "a@x.y"}
        }));
        assert!(matches!(
            result.err().expect("must be rejected"),
            EnvelopeConversionError::InvalidReplyTo(_)
        ));
    }

    #[test]
    fn at_most_ten_reply_to_entries_are_accepted() {
        let many = |n: usize| serde_json::json!({"reply_to": (0..n).map(|i| format!("r{i}@x.y")).collect::<Vec<_>>()});
        assert!(convert_with(many(10)).is_ok());
        assert!(matches!(
            convert_with(many(11)).err().expect("must be rejected"),
            EnvelopeConversionError::InvalidReplyTo(_)
        ));
    }

    #[test]
    fn repeated_header_name_in_raw_json_is_rejected() {
        let raw = r#"{"sender":"a@b.c","recipients":[{"kind":"to","address":"t@x.y"}],
            "body":{"kind":"plain","text":"hi"},"headers":{"X-A":"one","X-A":"two"}}"#;
        assert!(serde_json::from_str::<SubmitEmailRequest>(raw).is_err());
        let ok = r#"{"sender":"a@b.c","recipients":[{"kind":"to","address":"t@x.y"}],
            "body":{"kind":"plain","text":"hi"},"headers":{"X-A":"one","X-B":"two"}}"#;
        assert!(serde_json::from_str::<SubmitEmailRequest>(ok).is_ok());
    }

    #[test]
    fn newly_reserved_names_are_rejected_and_hints_stay_allowed() {
        for name in [
            "Received",
            "Authentication-Results",
            "Resent-From",
            "resent-date",
            "Return-Receipt-To",
            "Disposition-Notification-To",
        ] {
            assert!(
                convert_with(serde_json::json!({"headers": {name: "v"}})).is_err(),
                "{name}"
            );
        }
        for name in ["Precedence", "Auto-Submitted", "X-Mailer"] {
            assert!(
                convert_with(serde_json::json!({"headers": {name: "bulk"}})).is_ok(),
                "{name}"
            );
        }
    }

    #[test]
    fn reserved_headers_are_rejected_case_insensitively() {
        for name in [
            "From",
            "to",
            "CC",
            "Bcc",
            "Subject",
            "Date",
            "Message-ID",
            "MIME-Version",
            "Content-Type",
            "content-transfer-encoding",
            "Return-Path",
            "DKIM-Signature",
            "Reply-To",
            "Sender",
            "List-Unsubscribe",
            "List-Unsubscribe-Post",
        ] {
            let err = convert_with(serde_json::json!({"headers": {name: "v"}}))
                .err()
                .expect("must be rejected");
            assert!(
                matches!(err, EnvelopeConversionError::InvalidHeader(_)),
                "{name}"
            );
        }
    }

    #[test]
    fn at_most_ten_headers_are_accepted() {
        let headers = |n: usize| -> serde_json::Value {
            serde_json::json!({"headers": (0..n).map(|i| (format!("X-H{i}"), "v".to_owned())).collect::<std::collections::BTreeMap<_, _>>()})
        };
        assert!(convert_with(headers(10)).is_ok());
        assert!(convert_with(headers(11)).is_err());
    }

    #[test]
    fn plain_with_text_converts_to_plain_body() {
        let dto = BodyDto::Plain {
            text: Some("hello".into()),
            html: None,
        };
        assert!(matches!(
            BodySource::try_from(dto),
            Ok(BodySource::Plain(_))
        ));
    }

    #[test]
    fn plain_with_neither_text_nor_html_returns_invalid_plain() {
        let dto = BodyDto::Plain {
            text: None,
            html: None,
        };
        assert!(matches!(
            BodySource::try_from(dto),
            Err(BodyConversionError::InvalidPlain(_))
        ));
    }

    #[test]
    fn mjml_inline_converts_to_mjml_inline_source() {
        let dto = BodyDto::MjmlInline {
            source: "<mjml/>".into(),
        };
        assert!(matches!(
            BodySource::try_from(dto),
            Ok(BodySource::Mjml(MjmlSource::Inline(_)))
        ));
    }

    #[test]
    fn mjml_named_converts_to_mjml_named_source() {
        let dto = BodyDto::MjmlNamed {
            name: "welcome".into(),
        };
        assert!(matches!(
            BodySource::try_from(dto),
            Ok(BodySource::Mjml(MjmlSource::Named(_)))
        ));
    }

    #[test]
    fn mjml_remote_with_valid_url_converts_to_mjml_remote_source() {
        let dto = BodyDto::MjmlRemote {
            url: "https://example.com/template.mjml".into(),
        };
        assert!(matches!(
            BodySource::try_from(dto),
            Ok(BodySource::Mjml(MjmlSource::Remote(_)))
        ));
    }

    #[test]
    fn mjml_remote_with_invalid_url_returns_invalid_remote_url() {
        let dto = BodyDto::MjmlRemote {
            url: "not a url".into(),
        };
        assert!(matches!(
            BodySource::try_from(dto),
            Err(BodyConversionError::InvalidRemoteUrl(_))
        ));
    }

    #[test]
    fn invalid_sender_returns_error() {
        let req = SubmitEmailRequest {
            core: EnvelopeCoreDto {
                sender: MailboxDto::Text("not-an-email".into()),
                ..base_core()
            },
            ..base_request()
        };
        assert!(matches!(
            req.into_submit_input(),
            Err(EnvelopeConversionError::InvalidSender(_))
        ));
    }

    #[test]
    fn empty_recipients_returns_error() {
        let req = SubmitEmailRequest {
            core: EnvelopeCoreDto {
                recipients: vec![],
                ..base_core()
            },
            ..base_request()
        };
        assert!(matches!(
            req.into_submit_input(),
            Err(EnvelopeConversionError::EmptyRecipients)
        ));
    }

    #[test]
    fn invalid_recipient_returns_error() {
        let req = SubmitEmailRequest {
            core: EnvelopeCoreDto {
                recipients: vec![RecipientDto {
                    kind: RecipientKindDto::To,
                    address: "bad".into(),
                    name: None,
                }],
                ..base_core()
            },
            ..base_request()
        };
        assert!(matches!(
            req.into_submit_input(),
            Err(EnvelopeConversionError::InvalidRecipient(_))
        ));
    }

    #[test]
    fn json_with_every_field_and_unknown_field_deserializes_and_converts() {
        let raw = serde_json::json!({
            "idempotency_key": "k1",
            "correlation_id": "c1",
            "subject": "Hello",
            "sender": "a@b.c",
            "recipients": [{ "kind": "to", "address": "t@x.y" }],
            "body": { "kind": "plain", "text": "hi", "html": null },
            "variables": { "name": "Alice" },
            "attachments": [{
                "filename": "a.txt",
                "content_type": "text/plain",
                "inline_base64": "aGk="
            }],
            "unknown_field": true
        });

        let req: SubmitEmailRequest = serde_json::from_value(raw).unwrap();
        let input = req.into_submit_input().unwrap();

        assert_eq!(input.idempotency_key.as_deref(), Some("k1"));
        assert_eq!(input.correlation_id.as_deref(), Some("c1"));
        assert_eq!(input.subject.as_deref(), Some("Hello"));
        assert_eq!(input.variables["name"], "Alice");
        assert_eq!(input.attachments.len(), 1);
    }

    #[test]
    fn json_missing_required_field_is_a_deserialization_error() {
        let raw = serde_json::json!({ "sender": "a@b.c" });
        assert!(serde_json::from_value::<SubmitEmailRequest>(raw).is_err());
    }

    fn convert(
        core: EnvelopeCoreDto,
    ) -> Result<catapulte_domain::use_case::submit_email::SubmitEmailInput, EnvelopeConversionError>
    {
        SubmitEmailRequest {
            core,
            ..base_request()
        }
        .into_submit_input()
    }

    #[test]
    fn display_form_sender_is_stored_canonically() {
        let input = convert(EnvelopeCoreDto {
            sender: MailboxDto::Text("Acme <noreply@acme.com>".into()),
            ..base_core()
        })
        .unwrap();
        assert_eq!(input.sender, "Acme <noreply@acme.com>");
    }

    #[test]
    fn structured_sender_and_recipient_keep_names() {
        let raw = serde_json::json!({
            "sender": { "name": "Acme, \"Inc\"", "address": "noreply@acme.com" },
            "recipients": [{ "kind": "to", "name": "Alice", "address": "alice@x.y" }],
            "body": { "kind": "plain", "text": "hi", "html": null }
        });
        let req: SubmitEmailRequest = serde_json::from_value(raw).unwrap();
        let input = req.into_submit_input().unwrap();

        let sender: lettre::message::Mailbox = input.sender.parse().unwrap();
        assert_eq!(sender.name.as_deref(), Some("Acme, \"Inc\""));
        assert_eq!(input.recipients[0].1, "Alice <alice@x.y>");
    }

    #[test]
    fn invalid_sender_forms_are_rejected() {
        for sender in [
            MailboxDto::Text("Acme <not an address>".into()),
            MailboxDto::Structured {
                name: Some("Acme".into()),
                address: "Other <a@b.c>".into(),
            },
        ] {
            let result = convert(EnvelopeCoreDto {
                sender,
                ..base_core()
            });
            assert!(matches!(
                result,
                Err(EnvelopeConversionError::InvalidSender(_))
            ));
        }
    }

    #[test]
    fn recipient_with_name_requires_bare_address() {
        let result = convert(EnvelopeCoreDto {
            recipients: vec![RecipientDto {
                kind: RecipientKindDto::To,
                address: "Bob <bob@x.y>".into(),
                name: Some("Alice".into()),
            }],
            ..base_core()
        });
        assert!(matches!(
            result,
            Err(EnvelopeConversionError::InvalidRecipient(_))
        ));
    }

    #[test]
    fn non_ascii_name_round_trips_through_canonical_string() {
        let input = convert(EnvelopeCoreDto {
            sender: MailboxDto::Text("Café Ünïcode <a@b.com>".into()),
            ..base_core()
        })
        .unwrap();

        let reparsed: lettre::message::Mailbox = input.sender.parse().unwrap();

        assert_eq!(reparsed.name.as_deref(), Some("Café Ünïcode"));
        assert_eq!(reparsed.to_string(), input.sender);
        assert_eq!(reparsed.email.to_string(), "a@b.com");
    }

    fn sender_text(s: &str) -> Result<String, EnvelopeConversionError> {
        convert(EnvelopeCoreDto {
            sender: MailboxDto::Text(s.into()),
            ..base_core()
        })
        .map(|i| i.sender)
    }

    fn structured(name: &str, address: &str) -> MailboxDto {
        MailboxDto::Structured {
            name: Some(name.into()),
            address: address.into(),
        }
    }

    #[test]
    fn control_characters_in_names_are_rejected_without_panicking() {
        for name in ["a\r\nb", "a\nb", "a\0b", "a\tb", "\r\n"] {
            let sender = convert(EnvelopeCoreDto {
                sender: structured(name, "a@b.c"),
                ..base_core()
            });
            assert!(
                matches!(sender, Err(EnvelopeConversionError::InvalidSender(_))),
                "sender name {name:?}"
            );
            let recipient = convert(EnvelopeCoreDto {
                recipients: vec![RecipientDto {
                    kind: RecipientKindDto::To,
                    address: "a@b.c".into(),
                    name: Some(name.into()),
                }],
                ..base_core()
            });
            assert!(
                matches!(recipient, Err(EnvelopeConversionError::InvalidRecipient(_))),
                "recipient name {name:?}"
            );
        }
    }

    #[test]
    fn control_characters_in_text_form_names_are_rejected() {
        for name in ["\u{1}x", "x\u{7f}", "x\u{85}y"] {
            let text = format!("\"{name}\" <a@b.c>");
            let sender = convert(EnvelopeCoreDto {
                sender: MailboxDto::Text(text.clone()),
                ..base_core()
            });
            assert!(
                matches!(sender, Err(EnvelopeConversionError::InvalidSender(_))),
                "text sender {name:?}"
            );
            let sender = convert(EnvelopeCoreDto {
                sender: structured(name, "a@b.c"),
                ..base_core()
            });
            assert!(
                matches!(sender, Err(EnvelopeConversionError::InvalidSender(_))),
                "structured sender {name:?}"
            );
            for recipient in [
                RecipientDto {
                    kind: RecipientKindDto::To,
                    address: text.clone(),
                    name: None,
                },
                RecipientDto {
                    kind: RecipientKindDto::To,
                    address: "a@b.c".into(),
                    name: Some(name.into()),
                },
            ] {
                let result = convert(EnvelopeCoreDto {
                    recipients: vec![recipient],
                    ..base_core()
                });
                assert!(
                    matches!(result, Err(EnvelopeConversionError::InvalidRecipient(_))),
                    "recipient {name:?}"
                );
            }
        }
    }

    #[test]
    fn named_mailboxes_that_do_not_round_trip_are_rejected() {
        for address in [
            "\"a<b\"@example.com",
            "a@[127.0.0.1]",
            "\"a b\"@example.com",
        ] {
            let result = convert(EnvelopeCoreDto {
                sender: structured("Alice", address),
                ..base_core()
            });
            assert!(
                matches!(result, Err(EnvelopeConversionError::InvalidSender(_))),
                "{address}"
            );
        }
        assert!(sender_text("Alice <a@[127.0.0.1]>").is_err());
    }

    #[test]
    fn addresses_lettre_cannot_put_in_headers_are_rejected() {
        for address in [
            "a@[127.0.0.1]",
            "\"a b\"@example.com",
            "\"a<b\"@example.com",
        ] {
            assert!(sender_text(address).is_err(), "{address}");
        }
    }

    #[test]
    fn stored_names_never_hide_a_quoted_angle_in_the_address() {
        let stored = sender_text("\"A <b>\" <a@b.c>").unwrap();
        assert_eq!(
            catapulte_domain::entity::email::bare_address(&stored),
            "a@b.c"
        );
    }

    #[test]
    fn intake_output_equals_smtp_parse_then_format() {
        let fixtures = [
            MailboxDto::Text("a@b.c".into()),
            MailboxDto::Text("Acme <noreply@acme.com>".into()),
            MailboxDto::Text("Café Ünïcode <a@b.com>".into()),
            structured("Acme, \"Inc\"", "a@b.c"),
            structured("Ünï \\ back", "a@b.c"),
            structured("", "a@b.c"),
        ];
        for fixture in fixtures {
            let label = format!("{fixture:?}");
            let stored = super::validate_sender(fixture).unwrap();
            let delivered = catapulte_outbound_smtp::transport::parse_mailbox(&stored).unwrap();
            assert_eq!(delivered.to_string(), stored, "{label}");
        }
    }

    fn attachment_with(filename: &str, content_type: &str) -> SubmitEmailRequest {
        SubmitEmailRequest {
            attachments: vec![AttachmentDto {
                content_id: None,
                filename: filename.into(),
                content_type: content_type.into(),
                inline_base64: Some("aGk=".into()),
                url: None,
            }],
            ..base_request()
        }
    }

    #[test]
    fn invalid_attachment_content_types_are_rejected() {
        for ct in ["pdf", "application/pdf; x", ""] {
            let result = attachment_with("a.pdf", ct).into_submit_input();
            assert!(
                matches!(
                    result,
                    Err(EnvelopeConversionError::InvalidAttachmentContentType { .. })
                ),
                "{ct:?}"
            );
        }
    }

    #[test]
    fn content_type_with_parameters_is_accepted_unchanged() {
        let (_, ct) =
            super::validate_attachment_meta("a.txt", "text/plain; charset=utf-8").unwrap();
        assert_eq!(ct, "text/plain; charset=utf-8");
        assert!(
            attachment_with("a.txt", "text/plain; charset=utf-8")
                .into_submit_input()
                .is_ok()
        );
    }

    #[test]
    fn attachment_filenames_are_reduced_to_their_last_segment() {
        for (given, expected) in [
            ("../../invoice.pdf", "invoice.pdf"),
            ("C:\\x\\a.pdf", "a.pdf"),
            ("  report.pdf  ", "report.pdf"),
            ("plain.pdf", "plain.pdf"),
        ] {
            let (name, _) = super::validate_attachment_meta(given, "application/pdf").unwrap();
            assert_eq!(name, expected, "{given:?}");
        }
        let input = attachment_with("../../invoice.pdf", "application/pdf")
            .into_submit_input()
            .unwrap();
        let catapulte_domain::use_case::submit_email::AttachmentInput::Inline { filename, .. } =
            &input.attachments[0]
        else {
            panic!("expected Inline variant");
        };
        assert_eq!(filename, "invoice.pdf");
    }

    #[test]
    fn invalid_attachment_filenames_are_rejected() {
        let too_long = "a".repeat(256);
        for name in [
            "",
            "   ",
            "/",
            "dir/",
            ".",
            "..",
            "a/..",
            "invoice\u{202E}fdp.exe",
            "a\u{200B}b",
            "\u{FEFF}a",
            "a\u{200F}",
            "a\u{0}b",
            "a\nb",
            "a\u{7f}b",
            too_long.as_str(),
        ] {
            let result = super::validate_attachment_meta(name, "application/pdf");
            assert!(
                matches!(
                    result,
                    Err(EnvelopeConversionError::InvalidAttachmentFilename { .. })
                ),
                "{name:?}"
            );
        }
        let max = "a".repeat(255);
        assert!(super::validate_attachment_meta(&max, "application/pdf").is_ok());
    }

    #[test]
    fn remote_attachment_content_type_is_validated() {
        let req = SubmitEmailRequest {
            attachments: vec![AttachmentDto {
                content_id: None,
                filename: "remote.pdf".into(),
                content_type: "pdf".into(),
                inline_base64: None,
                url: Some("https://example.com/a.pdf".into()),
            }],
            ..base_request()
        };
        assert!(matches!(
            req.into_submit_input(),
            Err(EnvelopeConversionError::InvalidAttachmentContentType { .. })
        ));
    }

    #[test]
    fn valid_base64_attachment_decodes_correctly() {
        use base64::Engine;
        use catapulte_domain::use_case::submit_email::AttachmentInput;
        let content = b"hello attachment";
        let encoded = base64::engine::general_purpose::STANDARD.encode(content);
        let req = SubmitEmailRequest {
            attachments: vec![AttachmentDto {
                content_id: None,
                filename: "test.txt".into(),
                content_type: "text/plain".into(),
                inline_base64: Some(encoded),
                url: None,
            }],
            ..base_request()
        };
        let input = req.into_submit_input().unwrap();
        assert_eq!(input.attachments.len(), 1);
        let AttachmentInput::Inline { bytes, .. } = &input.attachments[0] else {
            panic!("expected Inline variant");
        };
        assert_eq!(bytes.as_ref(), content);
    }

    #[test]
    fn invalid_base64_attachment_returns_error() {
        let req = SubmitEmailRequest {
            attachments: vec![AttachmentDto {
                content_id: None,
                filename: "bad.txt".into(),
                content_type: "text/plain".into(),
                inline_base64: Some("not valid base64!!!".into()),
                url: None,
            }],
            ..base_request()
        };
        assert!(matches!(
            req.into_submit_input(),
            Err(EnvelopeConversionError::InvalidAttachmentBase64 { .. })
        ));
    }

    #[test]
    fn too_many_attachments_returns_error() {
        use super::MAX_ATTACHMENTS_PER_EMAIL;
        use base64::Engine;
        let encoded = base64::engine::general_purpose::STANDARD.encode(b"x");
        let attachments = (0..=MAX_ATTACHMENTS_PER_EMAIL)
            .map(|i| AttachmentDto {
                content_id: None,
                filename: format!("file{i}.txt"),
                content_type: "text/plain".into(),
                inline_base64: Some(encoded.clone()),
                url: None,
            })
            .collect();
        let req = SubmitEmailRequest {
            attachments,
            ..base_request()
        };
        assert!(matches!(
            req.into_submit_input(),
            Err(EnvelopeConversionError::TooManyAttachments)
        ));
    }

    #[test]
    fn oversize_attachment_returns_error() {
        use super::MAX_ATTACHMENT_BYTES;
        use base64::Engine;
        let big = vec![0u8; usize::try_from(MAX_ATTACHMENT_BYTES + 1).unwrap()];
        let encoded = base64::engine::general_purpose::STANDARD.encode(&big);
        let req = SubmitEmailRequest {
            attachments: vec![AttachmentDto {
                content_id: None,
                filename: "big.bin".into(),
                content_type: "application/octet-stream".into(),
                inline_base64: Some(encoded),
                url: None,
            }],
            ..base_request()
        };
        assert!(matches!(
            req.into_submit_input(),
            Err(EnvelopeConversionError::AttachmentTooLarge { .. })
        ));
    }

    #[test]
    fn overlong_invalid_base64_is_rejected_before_decoding() {
        use super::MAX_ATTACHMENT_BYTES;
        let len = usize::try_from(MAX_ATTACHMENT_BYTES.div_ceil(3) * 4 + 5).unwrap();
        let req = SubmitEmailRequest {
            attachments: vec![AttachmentDto {
                content_id: None,
                filename: "big.bin".into(),
                content_type: "application/octet-stream".into(),
                inline_base64: Some("!".repeat(len)),
                url: None,
            }],
            ..base_request()
        };
        assert!(matches!(
            req.into_submit_input(),
            Err(EnvelopeConversionError::AttachmentTooLarge { .. })
        ));
    }

    #[test]
    fn remote_url_attachment_builds_remote_variant() {
        use catapulte_domain::use_case::submit_email::AttachmentInput;
        let req = SubmitEmailRequest {
            attachments: vec![AttachmentDto {
                content_id: None,
                filename: "remote.pdf".into(),
                content_type: "application/pdf".into(),
                inline_base64: None,
                url: Some("https://example.com/file.pdf".into()),
            }],
            ..base_request()
        };
        let input = req.into_submit_input().unwrap();
        assert_eq!(input.attachments.len(), 1);
        assert!(matches!(
            input.attachments[0],
            AttachmentInput::Remote { .. }
        ));
    }

    #[test]
    fn both_inline_and_url_returns_invalid_shape() {
        use base64::Engine;
        let encoded = base64::engine::general_purpose::STANDARD.encode(b"x");
        let req = SubmitEmailRequest {
            attachments: vec![AttachmentDto {
                content_id: None,
                filename: "both.txt".into(),
                content_type: "text/plain".into(),
                inline_base64: Some(encoded),
                url: Some("https://example.com/file.txt".into()),
            }],
            ..base_request()
        };
        assert!(matches!(
            req.into_submit_input(),
            Err(EnvelopeConversionError::InvalidAttachmentShape { .. })
        ));
    }

    #[test]
    fn neither_inline_nor_url_returns_invalid_shape() {
        let req = SubmitEmailRequest {
            attachments: vec![AttachmentDto {
                content_id: None,
                filename: "neither.txt".into(),
                content_type: "text/plain".into(),
                inline_base64: None,
                url: None,
            }],
            ..base_request()
        };
        assert!(matches!(
            req.into_submit_input(),
            Err(EnvelopeConversionError::InvalidAttachmentShape { .. })
        ));
    }

    #[test]
    fn malformed_url_returns_invalid_attachment_url() {
        let req = SubmitEmailRequest {
            attachments: vec![AttachmentDto {
                content_id: None,
                filename: "bad-url.txt".into(),
                content_type: "text/plain".into(),
                inline_base64: None,
                url: Some("not a url at all".into()),
            }],
            ..base_request()
        };
        assert!(matches!(
            req.into_submit_input(),
            Err(EnvelopeConversionError::InvalidAttachmentUrl { .. })
        ));
    }
}

#[derive(Debug, Deserialize)]
pub struct ListEventsQuery {
    #[serde(default)]
    pub email_id: Option<String>,
    #[serde(default)]
    pub event_type: Option<String>,
    #[serde(default)]
    pub sender_name: Option<String>,
    #[serde(default)]
    pub error_class: Option<String>,
    #[serde(default)]
    pub after_ms: Option<i64>,
    #[serde(default)]
    pub before_ms: Option<i64>,
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub offset: Option<u32>,
}

pub const DEFAULT_EVENTS_LIMIT: u32 = 20;
pub const MAX_EVENTS_LIMIT: u32 = 100;

#[derive(Debug, Serialize)]
pub struct EventRecordDto {
    pub id: String,
    pub email_id: String,
    pub event_type: String,
    pub payload: Option<serde_json::Value>,
    pub sender_name: Option<String>,
    pub error_class: Option<String>,
    pub created_at_ms: i64,
}

impl From<catapulte_domain::port::event_repository::EventRecord> for EventRecordDto {
    fn from(r: catapulte_domain::port::event_repository::EventRecord) -> Self {
        Self {
            id: r.id.to_string(),
            email_id: r.email_id.as_uuid().to_string(),
            event_type: r.event_type,
            payload: r.payload,
            sender_name: r.sender_name.map(|n| n.as_str().to_owned()),
            error_class: r.error_class,
            created_at_ms: r.created_at_ms,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ListEventsResponse {
    pub events: Vec<EventRecordDto>,
    pub limit: u32,
    pub offset: u32,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmailStatusDto {
    Queued,
    Sent,
    Failed,
}

impl From<EmailStatusDto> for catapulte_domain::port::email_repository::EmailStatus {
    fn from(s: EmailStatusDto) -> Self {
        match s {
            EmailStatusDto::Queued => Self::Queued,
            EmailStatusDto::Sent => Self::Sent,
            EmailStatusDto::Failed => Self::Failed,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct ListEmailsQuery {
    #[serde(default)]
    pub status: Option<EmailStatusDto>,
    #[serde(default)]
    pub after_ms: Option<i64>,
    #[serde(default)]
    pub before_ms: Option<i64>,
    #[serde(default)]
    pub recipient: Option<String>,
    #[serde(default)]
    pub template: Option<String>,
    #[serde(default)]
    pub tag: Option<String>,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub offset: Option<u32>,
}

pub const DEFAULT_EMAILS_LIMIT: u32 = 20;
pub const MAX_EMAILS_LIMIT: u32 = 100;

#[derive(Debug, Serialize)]
pub struct RecipientResponseDto {
    pub kind: String,
    pub address: String,
}

#[derive(Debug, Serialize)]
pub struct EmailRecordDto {
    pub id: String,
    pub idempotency_key: Option<String>,
    pub subject: Option<String>,
    pub sender: String,
    pub recipients: Vec<RecipientResponseDto>,
    pub created_at_ms: i64,
    pub status: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub metadata: std::collections::BTreeMap<String, String>,
}

impl From<catapulte_domain::port::email_repository::EmailRecord> for EmailRecordDto {
    fn from(r: catapulte_domain::port::email_repository::EmailRecord) -> Self {
        use catapulte_domain::entity::email::RecipientKind;
        use catapulte_domain::port::email_repository::EmailStatus;
        let status = match r.status {
            EmailStatus::Queued => "queued",
            EmailStatus::Sent => "sent",
            EmailStatus::Failed => "failed",
        };
        let recipients = r
            .recipients
            .into_iter()
            .map(|(kind, address)| RecipientResponseDto {
                kind: match kind {
                    RecipientKind::To => "to".to_owned(),
                    RecipientKind::Cc => "cc".to_owned(),
                    RecipientKind::Bcc => "bcc".to_owned(),
                },
                address,
            })
            .collect();
        Self {
            id: r.id.as_uuid().to_string(),
            idempotency_key: r.idempotency_key,
            subject: r.subject,
            sender: r.sender,
            recipients,
            created_at_ms: r.created_at_ms,
            status: status.to_owned(),
            tags: r.tags,
            metadata: r.metadata,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ListEmailsResponse {
    pub emails: Vec<EmailRecordDto>,
    pub limit: u32,
    pub offset: u32,
}

#[derive(Serialize)]
pub struct SenderQuotaDto {
    pub count: u64,
    pub range: String,
}

#[derive(Serialize)]
pub struct SenderDto {
    pub name: String,
    pub sent_in_range: u64,
    pub failed_in_range: u64,
    pub quota: Option<SenderQuotaDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub match_sender_domain: Option<String>,
}

#[derive(Serialize)]
pub struct ListSendersResponse {
    pub senders: Vec<SenderDto>,
}
