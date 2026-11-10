use anyhow::Context;
use catapulte_domain::entity::attachment::ResolvedAttachment;
use catapulte_domain::entity::body::RenderedBody;
use catapulte_domain::entity::email::RecipientKind;
use catapulte_domain::port::email_sender::OutboundEmail;
use catapulte_domain::port::email_transport::{
    DeliveryReceipt, EmailTransport, TransportError, TransportErrorKind,
};
use lettre::message::header::{ContentDisposition, ContentType};
use lettre::message::{Attachment, Mailbox, MultiPart, SinglePart};
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SmtpTls {
    Starttls,
    Tls,
    None,
}

pub struct SmtpConfig {
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub password: Option<String>,
    pub tls: SmtpTls,
}

fn parse_port(raw: Option<String>, key: &str) -> anyhow::Result<u16> {
    match raw {
        Some(val) => val
            .parse::<u16>()
            .with_context(|| format!("invalid value for env var {key}")),
        None => Ok(587),
    }
}

fn parse_tls_str(val: &str, key: &str) -> anyhow::Result<SmtpTls> {
    match val {
        "starttls" => Ok(SmtpTls::Starttls),
        "tls" => Ok(SmtpTls::Tls),
        "none" => Ok(SmtpTls::None),
        other => anyhow::bail!("unknown value for env var {key}: {other}"),
    }
}

fn parse_tls(raw: Option<String>, key: &str) -> anyhow::Result<SmtpTls> {
    match raw {
        None => Ok(SmtpTls::Starttls),
        Some(val) => parse_tls_str(&val, key),
    }
}

/// Must stay identical to the HTTP intake's `parse_mailbox`, so delivery parses exactly
/// what intake accepted.
///
/// # Errors
///
/// Returns an error if `addr` is neither a mailbox nor a bare address.
pub fn parse_mailbox(addr: &str) -> anyhow::Result<Mailbox> {
    addr.parse::<Mailbox>().or_else(|_| {
        addr.parse::<lettre::Address>()
            .map(|a| Mailbox::new(None, a))
            .with_context(|| format!("invalid address: {addr}"))
    })
}

fn add_recipient(
    builder: lettre::message::MessageBuilder,
    kind: RecipientKind,
    mailbox: Mailbox,
) -> lettre::message::MessageBuilder {
    match kind {
        RecipientKind::To => builder.to(mailbox),
        RecipientKind::Cc => builder.cc(mailbox),
        RecipientKind::Bcc => builder.bcc(mailbox),
    }
}

fn apply_recipients(
    mut builder: lettre::message::MessageBuilder,
    recipients: &[(RecipientKind, String)],
) -> anyhow::Result<lettre::message::MessageBuilder> {
    for (kind, address) in recipients {
        let mailbox = parse_mailbox(address)?;
        builder = add_recipient(builder, *kind, mailbox);
    }
    Ok(builder)
}

fn apply_reply_to_and_headers(
    mut builder: lettre::message::MessageBuilder,
    reply_to: &[String],
    headers: &[(String, String)],
) -> anyhow::Result<lettre::message::MessageBuilder> {
    use lettre::message::header::{HeaderName, HeaderValue};
    for address in reply_to {
        builder = builder.reply_to(parse_mailbox(address)?);
    }
    for (name, value) in headers {
        let name = HeaderName::new_from_ascii(name.clone())
            .map_err(|_| anyhow::anyhow!("invalid header name: {name}"))?;
        builder = builder.raw_header(HeaderValue::new(name, value.clone()));
    }
    Ok(builder)
}

/// Rejects a message whose top-level header block has a physical line over 998 characters
/// (RFC 5322). lettre folds only at whitespace, so a long unbroken value stays one line.
fn check_header_line_lengths(raw: &[u8]) -> anyhow::Result<()> {
    let end = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .unwrap_or(raw.len());
    let longest = raw[..end]
        .split(|b| *b == b'\n')
        .map(|line| line.strip_suffix(b"\r").unwrap_or(line).len())
        .max()
        .unwrap_or(0);
    anyhow::ensure!(longest <= 998, "a header line exceeds 998 characters");
    Ok(())
}

fn build_body_part(body: &RenderedBody) -> MultiPart {
    match (body.text(), body.html()) {
        (Some(text), Some(html)) => MultiPart::alternative()
            .singlepart(
                SinglePart::builder()
                    .header(ContentType::TEXT_PLAIN)
                    .body(text.to_owned()),
            )
            .singlepart(
                SinglePart::builder()
                    .header(ContentType::TEXT_HTML)
                    .body(html.to_owned()),
            ),
        (Some(text), None) => MultiPart::alternative().singlepart(
            SinglePart::builder()
                .header(ContentType::TEXT_PLAIN)
                .body(text.to_owned()),
        ),
        (None, Some(html)) => MultiPart::alternative().singlepart(
            SinglePart::builder()
                .header(ContentType::TEXT_HTML)
                .body(html.to_owned()),
        ),
        (None, None) => {
            unreachable!("Plain invariant: at least one of text or html must be provided")
        }
    }
}

fn parse_attachment_type(att: &ResolvedAttachment) -> anyhow::Result<ContentType> {
    ContentType::parse(&att.content_type)
        .with_context(|| format!("invalid attachment content-type: {}", att.content_type))
}

fn finalize_message(
    builder: lettre::message::MessageBuilder,
    subject: Option<&str>,
    body: &RenderedBody,
    attachments: &[ResolvedAttachment],
) -> anyhow::Result<Message> {
    let builder = match subject {
        Some(s) => builder.subject(s),
        None => builder,
    };

    // Without an HTML body nothing can reference a cid, so inline attachments are sent as
    // regular ones.
    let has_inline = body.html().is_some() && attachments.iter().any(|a| a.content_id.is_some());

    if attachments.is_empty() {
        let msg = match (body.text(), body.html()) {
            (Some(text), Some(html)) => builder.multipart(
                MultiPart::alternative()
                    .singlepart(
                        SinglePart::builder()
                            .header(ContentType::TEXT_PLAIN)
                            .body(text.to_owned()),
                    )
                    .singlepart(
                        SinglePart::builder()
                            .header(ContentType::TEXT_HTML)
                            .body(html.to_owned()),
                    ),
            ),
            (Some(text), None) => builder.singlepart(
                SinglePart::builder()
                    .header(ContentType::TEXT_PLAIN)
                    .body(text.to_owned()),
            ),
            (None, Some(html)) => builder.singlepart(
                SinglePart::builder()
                    .header(ContentType::TEXT_HTML)
                    .body(html.to_owned()),
            ),
            (None, None) => {
                unreachable!("Plain invariant: at least one of text or html must be provided")
            }
        };
        msg.context("building email message")
    } else {
        let body_part = build_body_part(body);
        let mut regular = Vec::new();
        let first = if has_inline {
            let mut related = MultiPart::related().multipart(body_part);
            for att in attachments {
                match &att.content_id {
                    Some(cid) => {
                        let content_type = parse_attachment_type(att)?;
                        related = related.singlepart(
                            Attachment::new_inline_with_name(cid.clone(), att.filename.clone())
                                .body(att.bytes.to_vec(), content_type),
                        );
                    }
                    None => regular.push(att),
                }
            }
            if regular.is_empty() {
                return builder
                    .multipart(related)
                    .context("building email message with inline attachments");
            }
            related
        } else {
            regular.extend(attachments);
            body_part
        };
        let mut mixed = MultiPart::mixed().multipart(first);
        for att in regular {
            let content_type = parse_attachment_type(att)?;
            mixed = mixed.singlepart(
                SinglePart::builder()
                    .header(content_type)
                    .header(ContentDisposition::attachment(&att.filename))
                    .body(att.bytes.to_vec()),
            );
        }
        builder
            .multipart(mixed)
            .context("building email message with attachments")
    }
}

type TransportBuilder = lettre::transport::smtp::AsyncSmtpTransportBuilder;

fn build_transport_builder(tls: &SmtpTls, host: &str) -> anyhow::Result<TransportBuilder> {
    match tls {
        SmtpTls::Starttls => AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(host)
            .context("building smtp transport"),
        SmtpTls::Tls => {
            AsyncSmtpTransport::<Tokio1Executor>::relay(host).context("building smtp transport")
        }
        SmtpTls::None => Ok(AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(
            host,
        )),
    }
}

fn apply_credentials(
    builder: TransportBuilder,
    username: Option<String>,
    password: Option<String>,
) -> TransportBuilder {
    if let (Some(u), Some(p)) = (username, password) {
        builder.credentials(Credentials::new(u, p))
    } else {
        builder
    }
}

impl SmtpConfig {
    /// # Errors
    ///
    /// Returns an error if a required environment variable is missing or has an invalid value.
    pub fn from_env(prefix: &str) -> anyhow::Result<Self> {
        Self::from_lookup(prefix, |key| std::env::var(key))
    }

    pub(crate) fn from_lookup<F>(prefix: &str, lookup: F) -> anyhow::Result<Self>
    where
        F: Fn(&str) -> Result<String, std::env::VarError>,
    {
        let host_key = format!("{prefix}_HOST");
        let host = lookup(&host_key).with_context(|| format!("missing env var {host_key}"))?;
        let port = parse_port(
            lookup(&format!("{prefix}_PORT")).ok(),
            &format!("{prefix}_PORT"),
        )?;
        let username = lookup(&format!("{prefix}_USERNAME")).ok();
        let password = lookup(&format!("{prefix}_PASSWORD")).ok();
        let tls = parse_tls(
            lookup(&format!("{prefix}_TLS")).ok(),
            &format!("{prefix}_TLS"),
        )?;
        Ok(Self {
            host,
            port,
            username,
            password,
            tls,
        })
    }

    /// # Errors
    ///
    /// Returns an error if the SMTP transport cannot be built.
    pub fn build(self) -> anyhow::Result<SmtpTransport> {
        let builder = build_transport_builder(&self.tls, &self.host)?.port(self.port);
        let builder = apply_credentials(builder, self.username, self.password);
        let transport = builder.build();
        Ok(SmtpTransport {
            transport,
            max_message_bytes: None,
        })
    }
}

pub struct SmtpTransport {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    max_message_bytes: Option<u64>,
}

/// Message-level rejections (enhanced codes `5.1.x`, `5.2.x`, `5.6.x`) cannot succeed on any
/// route; every other permanent failure is route-specific. `text` is the lettre error text,
/// where the enhanced code is the first word after `permanent error (NNN): `.
#[must_use]
pub(crate) fn classify(permanent: bool, text: &str) -> TransportErrorKind {
    if !permanent {
        return TransportErrorKind::Transient;
    }
    let reply = text.split_once("): ").map_or(text, |(_, rest)| rest);
    let enhanced = reply.split_whitespace().next().and_then(|word| {
        let mut parts = word.split('.');
        match (parts.next(), parts.next(), parts.next(), parts.next()) {
            (Some("5"), Some(subject), Some(detail), None)
                if [subject, detail].iter().all(|p| {
                    (1..=3).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_digit())
                }) =>
            {
                Some((subject, detail))
            }
            _ => None,
        }
    });
    match enhanced {
        Some(("1" | "2" | "6", _)) => TransportErrorKind::PermanentMessage,
        _ => TransportErrorKind::PermanentRoute,
    }
}

const MAX_REPLY_CHARS: usize = 256;

/// Caps remote-controlled reply text, ellipsis included, at 256 characters before it reaches
/// events and logs.
fn truncate_reply(text: &str) -> String {
    if text.chars().count() <= MAX_REPLY_CHARS {
        return text.to_owned();
    }
    let kept: String = text.chars().take(MAX_REPLY_CHARS - 1).collect();
    format!("{kept}…")
}

/// `"{code} {lines joined by space}"`, capped because the relay controls the text.
fn format_smtp_response<'a>(
    code: impl std::fmt::Display,
    lines: impl Iterator<Item = &'a str>,
) -> String {
    let joined = lines.collect::<Vec<_>>().join(" ");
    truncate_reply(&format!("{code} {joined}"))
}

/// Classifies on the full text, then caps what is kept, so truncation cannot hide the code.
fn smtp_text_error(permanent: bool, text: &str) -> TransportError {
    TransportError {
        kind: classify(permanent, text),
        source: anyhow::anyhow!("{}", truncate_reply(text)),
    }
}

fn message_error(e: anyhow::Error) -> TransportError {
    TransportError {
        kind: TransportErrorKind::PermanentMessage,
        source: e,
    }
}

impl SmtpTransport {
    /// Rejects, as a message-level failure, any rendered message larger than `max` bytes.
    #[must_use]
    pub fn with_max_message_bytes(mut self, max: u64) -> Self {
        self.max_message_bytes = Some(max);
        self
    }

    /// Builds the message and its wire bytes, enforcing the size limit on the final bytes.
    fn prepare(&self, email: &OutboundEmail) -> Result<(Message, Vec<u8>), TransportError> {
        let from = parse_mailbox(&email.sender).map_err(message_error)?;
        let builder = Message::builder()
            .from(from)
            .message_id(Some(email.message_id.clone()));
        let builder = apply_recipients(builder, &email.recipients)
            .and_then(|b| apply_reply_to_and_headers(b, &email.reply_to, &email.headers))
            .map_err(message_error)?;
        let message = finalize_message(
            builder,
            email.subject.as_deref(),
            &email.body,
            &email.attachments,
        )
        .map_err(message_error)?;
        let raw = message.formatted();
        check_header_line_lengths(&raw).map_err(message_error)?;
        if self
            .max_message_bytes
            .is_some_and(|max| raw.len() as u64 > max)
        {
            return Err(message_error(anyhow::anyhow!(
                "rendered message exceeds CATAPULTE_MAX_MESSAGE_BYTES"
            )));
        }
        Ok((message, raw))
    }

    pub(crate) async fn send_inner(
        &self,
        email: &OutboundEmail,
    ) -> Result<DeliveryReceipt, TransportError> {
        let (message, raw) = self.prepare(email)?;
        let response = self
            .transport
            .send_raw(message.envelope(), &raw)
            .await
            .map_err(|e| smtp_text_error(e.is_permanent(), &e.to_string()))?;
        Ok(DeliveryReceipt {
            smtp_response: Some(format_smtp_response(response.code(), response.message())),
        })
    }
}

impl EmailTransport for SmtpTransport {
    async fn deliver<'a>(
        &'a self,
        email: &'a OutboundEmail,
    ) -> Result<DeliveryReceipt, TransportError> {
        self.send_inner(email).await
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::env::VarError;

    use catapulte_domain::entity::attachment::ResolvedAttachment;
    use catapulte_domain::entity::body::{Plain, RenderedBody};
    use catapulte_domain::entity::email::RecipientKind;
    use lettre::Address;

    use super::{
        SmtpConfig, SmtpTls, TransportErrorKind, classify, finalize_message, message_error,
        parse_mailbox, parse_port, parse_tls, smtp_text_error, truncate_reply,
    };

    fn make_lookup(
        vars: HashMap<&'static str, &'static str>,
    ) -> impl Fn(&str) -> Result<String, VarError> {
        move |key: &str| {
            vars.get(key)
                .map(|v| (*v).to_owned())
                .ok_or(VarError::NotPresent)
        }
    }

    fn base_builder() -> lettre::message::MessageBuilder {
        lettre::Message::builder()
            .from("from@example.com".parse::<Address>().unwrap().into())
            .to("to@example.com".parse::<Address>().unwrap().into())
    }

    fn plain_text_body() -> RenderedBody {
        let plain = Plain::try_new(Some("hello".to_string()), None).unwrap();
        RenderedBody::new(plain)
    }

    fn multipart_body() -> RenderedBody {
        let plain =
            Plain::try_new(Some("text".to_string()), Some("<p>html</p>".to_string())).unwrap();
        RenderedBody::new(plain)
    }

    #[test]
    fn smtp_tls_from_env_parses_variants() {
        let cases = [
            ("starttls", SmtpTls::Starttls),
            ("tls", SmtpTls::Tls),
            ("none", SmtpTls::None),
        ];

        for (tls_val, expected) in cases {
            let mut vars = HashMap::new();
            vars.insert("TEST_TLS_HOST", "localhost");
            vars.insert("TEST_TLS_TLS", tls_val);
            let config = SmtpConfig::from_lookup("TEST_TLS", make_lookup(vars)).unwrap();
            assert_eq!(config.tls, expected);
        }
    }

    #[test]
    fn smtp_tls_from_env_defaults_to_starttls() {
        let mut vars = HashMap::new();
        vars.insert("TEST_DEFAULT_TLS_HOST", "localhost");
        // TEST_DEFAULT_TLS_TLS intentionally absent
        let config = SmtpConfig::from_lookup("TEST_DEFAULT_TLS", make_lookup(vars)).unwrap();
        assert_eq!(config.tls, SmtpTls::Starttls);
    }

    #[test]
    fn smtp_config_from_env_missing_host_returns_error() {
        let mut vars = HashMap::new();
        vars.insert("SMTP_PORT", "587");
        // SMTP_HOST intentionally absent
        let result = SmtpConfig::from_lookup("SMTP", make_lookup(vars));
        assert!(result.is_err());
    }

    #[test]
    fn parse_port_defaults_to_587() {
        assert_eq!(parse_port(None, "X_PORT").unwrap(), 587);
    }

    #[test]
    fn parse_port_parses_valid_value() {
        assert_eq!(parse_port(Some("465".to_string()), "X_PORT").unwrap(), 465);
    }

    #[test]
    fn parse_port_rejects_invalid_value() {
        assert!(parse_port(Some("not-a-port".to_string()), "X_PORT").is_err());
    }

    #[test]
    fn parse_tls_defaults_to_starttls() {
        assert_eq!(parse_tls(None, "X_TLS").unwrap(), SmtpTls::Starttls);
    }

    #[test]
    fn parse_tls_rejects_unknown_value() {
        assert!(parse_tls(Some("unknown".to_string()), "X_TLS").is_err());
    }

    #[tokio::test]
    async fn smtp_config_build_no_tls_succeeds() {
        let config = SmtpConfig {
            host: "localhost".to_string(),
            port: 25,
            username: None,
            password: None,
            tls: SmtpTls::None,
        };
        assert!(config.build().is_ok());
    }

    #[test]
    fn finalize_message_text_only() {
        let body = plain_text_body();
        assert!(finalize_message(base_builder(), Some("Test Subject"), &body, &[]).is_ok());
    }

    #[test]
    fn finalize_message_html_only() {
        let plain = Plain::try_new(None, Some("<p>hi</p>".to_string())).unwrap();
        let body = RenderedBody::new(plain);
        assert!(finalize_message(base_builder(), None, &body, &[]).is_ok());
    }

    #[test]
    fn finalize_message_multipart() {
        let body = multipart_body();
        assert!(finalize_message(base_builder(), Some("Test Subject"), &body, &[]).is_ok());
    }

    #[test]
    fn finalize_message_with_one_attachment_produces_message() {
        let body = plain_text_body();
        let att = ResolvedAttachment {
            filename: "test.txt".to_owned(),
            content_type: "text/plain".to_owned(),
            bytes: bytes::Bytes::from_static(b"hello attachment"),
            content_id: None,
        };
        assert!(finalize_message(base_builder(), Some("With Attachment"), &body, &[att]).is_ok());
    }

    #[test]
    fn finalize_message_with_multiple_attachments_and_multipart_body_produces_message() {
        let body = multipart_body();
        let att1 = ResolvedAttachment {
            filename: "a.txt".to_owned(),
            content_type: "text/plain".to_owned(),
            bytes: bytes::Bytes::from_static(b"file a"),
            content_id: None,
        };
        let att2 = ResolvedAttachment {
            filename: "b.txt".to_owned(),
            content_type: "text/plain".to_owned(),
            bytes: bytes::Bytes::from_static(b"file b"),
            content_id: None,
        };
        assert!(finalize_message(base_builder(), Some("Multi"), &body, &[att1, att2]).is_ok());
    }

    fn cid_att(name: &str, cid: Option<&str>) -> ResolvedAttachment {
        ResolvedAttachment {
            filename: name.to_owned(),
            content_type: "image/png".to_owned(),
            bytes: bytes::Bytes::from_static(b"png"),
            content_id: cid.map(str::to_owned),
        }
    }

    fn raw_message(body: &RenderedBody, atts: &[ResolvedAttachment]) -> String {
        let msg = finalize_message(base_builder(), Some("s"), body, atts).unwrap();
        String::from_utf8(msg.formatted()).unwrap()
    }

    #[test]
    fn inline_attachment_with_html_builds_related_only() {
        let raw = raw_message(&multipart_body(), &[cid_att("logo.png", Some("logo"))]);
        assert!(raw.contains("multipart/related"));
        assert!(!raw.contains("multipart/mixed"));
        assert!(raw.contains("Content-ID: <logo>"));
        assert!(raw.contains("Content-Disposition: inline"));
    }

    struct Part {
        headers: String,
        children: Vec<Part>,
    }

    impl Part {
        fn parse(raw: &str) -> Self {
            let (headers, body) = raw.split_once("\r\n\r\n").unwrap_or((raw, ""));
            let headers = headers.replace("\r\n ", " ").replace("\r\n\t", " ");
            let mut children = Vec::new();
            if let Some(boundary) = headers
                .split("boundary=\"")
                .nth(1)
                .and_then(|r| r.split('"').next())
            {
                let delim = format!("--{boundary}");
                let mut sections = body.split(delim.as_str()).skip(1).collect::<Vec<_>>();
                sections.pop();
                children = sections
                    .into_iter()
                    .map(|c| Self::parse(c.strip_prefix("\r\n").unwrap_or(c)))
                    .collect();
            }
            Self { headers, children }
        }

        fn kind(&self) -> &str {
            let ct = self.headers.split("Content-Type: ").nth(1).unwrap_or("");
            ct.split([';', '\r']).next().unwrap_or("")
        }

        fn shape(&self) -> String {
            if self.children.is_empty() {
                return self.kind().to_owned();
            }
            let inner: Vec<_> = self.children.iter().map(Self::shape).collect();
            format!("{}[{}]", self.kind(), inner.join(","))
        }
    }

    #[test]
    fn inline_and_regular_attachments_build_mixed_wrapping_related() {
        let raw = raw_message(
            &multipart_body(),
            &[cid_att("logo.png", Some("logo")), cid_att("b.png", None)],
        );
        let root = Part::parse(&raw);
        assert_eq!(
            root.shape(),
            "multipart/mixed[multipart/related[multipart/alternative[text/plain,text/html],\
             image/png],image/png]"
        );
        let inline = &root.children[0].children[1];
        assert!(inline.headers.contains("Content-ID: <logo>"));
        assert!(inline.headers.contains("Content-Disposition: inline"));
        assert!(inline.headers.contains("filename=\"logo.png\""));
        assert!(root.children[1].headers.contains("filename=\"b.png\""));
    }

    #[test]
    fn inline_attachment_with_html_only_body_builds_related_of_html_and_image() {
        let plain = Plain::try_new(None, Some("<img src=\"cid:logo\">".to_string())).unwrap();
        let raw = raw_message(
            &RenderedBody::new(plain),
            &[cid_att("logo.png", Some("logo"))],
        );
        let root = Part::parse(&raw);
        assert_eq!(
            root.shape(),
            "multipart/related[multipart/alternative[text/html],image/png]"
        );
    }

    #[test]
    fn inline_attachment_without_html_falls_back_to_regular() {
        let raw = raw_message(&plain_text_body(), &[cid_att("logo.png", Some("logo"))]);
        assert!(raw.contains("multipart/mixed"));
        assert!(!raw.contains("multipart/related"));
        assert!(!raw.contains("Content-ID"));
        assert!(raw.contains("filename=\"logo.png\""));
    }

    #[test]
    fn finalize_message_with_invalid_content_type_returns_error() {
        let body = plain_text_body();
        let att = ResolvedAttachment {
            filename: "bad.bin".to_owned(),
            content_type: "not a content type///".to_owned(),
            bytes: bytes::Bytes::from_static(b"data"),
            content_id: None,
        };
        assert!(finalize_message(base_builder(), None, &body, &[att]).is_err());
    }

    #[test]
    fn parse_mailbox_valid_address() {
        assert!(parse_mailbox("user@example.com").is_ok());
    }

    #[test]
    fn parse_mailbox_invalid_address() {
        assert!(parse_mailbox("not-an-email").is_err());
    }

    #[test]
    fn display_name_mailbox_is_kept_and_rfc2047_encoded_in_from() {
        let mailbox = parse_mailbox("Café Ünïcode <a@b.com>").unwrap();
        let msg = finalize_message(
            lettre::Message::builder()
                .from(mailbox)
                .to("to@example.com".parse::<Address>().unwrap().into()),
            None,
            &plain_text_body(),
            &[],
        )
        .unwrap();

        let raw = String::from_utf8(msg.formatted()).unwrap();
        let from = raw.lines().find(|l| l.starts_with("From:")).unwrap();
        assert!(
            from.contains("=?utf-8?"),
            "From should be RFC 2047 encoded: {from}"
        );
        assert!(from.contains("<a@b.com>"), "{from}");
        assert!(from.is_ascii(), "{from}");
    }

    #[test]
    fn parse_mailbox_rejects_unparseable_value() {
        assert!(parse_mailbox("not an address").is_err());
    }

    #[test]
    fn classify_table() {
        use TransportErrorKind::{PermanentMessage, PermanentRoute, Transient};

        let cases = [
            (false, "transient error (451): 4.3.0 try later", Transient),
            (
                false,
                "transient error (421): service unavailable",
                Transient,
            ),
            (
                true,
                "permanent error (550): 5.1.1 user unknown",
                PermanentMessage,
            ),
            (
                true,
                "permanent error (553): 5.1.3 bad mailbox syntax",
                PermanentMessage,
            ),
            (
                true,
                "permanent error (552): 5.2.2 mailbox full",
                PermanentMessage,
            ),
            (
                true,
                "permanent error (552): 5.3.4 message too large",
                PermanentRoute,
            ),
            (
                true,
                "permanent error (554): 5.6.1 media not supported",
                PermanentMessage,
            ),
            (
                true,
                "permanent error (535): 5.7.8 authentication failed",
                PermanentRoute,
            ),
            (
                true,
                "permanent error (550): 5.7.1 relay denied",
                PermanentRoute,
            ),
            (
                true,
                "permanent error (554): 5.3.0 other mail system",
                PermanentRoute,
            ),
            (
                true,
                "permanent error (550): mailbox unavailable",
                PermanentRoute,
            ),
            (true, "permanent error (535): ", PermanentRoute),
            (true, "5.1.1 user unknown", PermanentMessage),
            (
                true,
                "permanent error (550): 5.1.1.1 too many parts",
                PermanentRoute,
            ),
            (
                true,
                "permanent error (550): 4.1.1 wrong class",
                PermanentRoute,
            ),
        ];
        for (permanent, text, expected) in cases {
            assert_eq!(classify(permanent, text), expected, "{text}");
        }
    }

    #[test]
    fn message_build_errors_are_permanent_message() {
        let err = message_error(anyhow::anyhow!("invalid address"));
        assert_eq!(err.kind, TransportErrorKind::PermanentMessage);
    }

    #[test]
    fn truncate_reply_caps_on_a_char_boundary() {
        assert_eq!(truncate_reply("short"), "short");
        let long = "é".repeat(300);
        let cut = truncate_reply(&long);
        assert_eq!(cut.chars().count(), 256);
        assert!(cut.ends_with('…'));
        assert!(cut.starts_with(&"é".repeat(255)));
        assert_eq!(truncate_reply(&"é".repeat(256)), "é".repeat(256));
    }

    #[test]
    fn classification_uses_the_full_text_before_truncation() {
        let text = format!("{}): 5.1.1 no such user", "x".repeat(300));

        let err = smtp_text_error(true, &text);

        assert_eq!(err.kind, TransportErrorKind::PermanentMessage);
        assert_eq!(err.source.to_string().chars().count(), 256);
    }

    fn local_transport(max: Option<u64>) -> super::SmtpTransport {
        let transport = SmtpConfig {
            host: "localhost".into(),
            port: 2525,
            username: None,
            password: None,
            tls: SmtpTls::None,
        }
        .build()
        .unwrap();
        match max {
            Some(max) => transport.with_max_message_bytes(max),
            None => transport,
        }
    }

    fn email_with_attachment(
        size: usize,
        kind: RecipientKind,
    ) -> catapulte_domain::port::email_sender::OutboundEmail {
        catapulte_domain::port::email_sender::OutboundEmail {
            sender: "from@example.com".into(),
            subject: Some("big".into()),
            recipients: vec![(kind, "to@example.com".into())],
            body: plain_text_body(),
            attachments: vec![ResolvedAttachment {
                content_id: None,
                filename: "big.bin".into(),
                content_type: "application/octet-stream".into(),
                bytes: bytes::Bytes::from(vec![7u8; size]),
            }],
            reply_to: vec![],
            headers: vec![],
            message_id: "<018f4e3c-2d1a-7b3c-8f00-1234567890ab@example.com>".into(),
        }
    }

    #[tokio::test]
    async fn rendered_message_over_max_is_a_message_level_failure() {
        let transport = local_transport(Some(1000));

        let err = transport
            .prepare(&email_with_attachment(2000, RecipientKind::To))
            .unwrap_err();

        assert_eq!(err.kind, TransportErrorKind::PermanentMessage);
        assert!(err.to_string().contains("CATAPULTE_MAX_MESSAGE_BYTES"));
    }

    #[tokio::test]
    async fn rendered_message_within_max_is_prepared() {
        let transport = local_transport(Some(100_000));

        let (_, raw) = transport
            .prepare(&email_with_attachment(2000, RecipientKind::To))
            .unwrap();

        assert!(raw.len() <= 100_000);
    }

    #[tokio::test]
    async fn bcc_recipient_stays_in_envelope_but_not_in_wire_bytes() {
        // send_raw(message.envelope(), &raw) is what `send` does internally.
        let transport = local_transport(None);

        let (message, raw) = transport
            .prepare(&email_with_attachment(10, RecipientKind::Bcc))
            .unwrap();

        let to: Vec<String> = message
            .envelope()
            .to()
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(to, vec!["to@example.com".to_owned()]);
        let raw = String::from_utf8_lossy(&raw);
        assert!(!raw.contains("Bcc:"), "Bcc header must not be on the wire");
    }

    #[tokio::test]
    async fn huge_rendered_body_over_max_is_a_message_level_failure() {
        let transport = local_transport(Some(1000));
        let mut email = email_with_attachment(0, RecipientKind::To);
        email.attachments.clear();
        email.body = RenderedBody::new(
            Plain::try_new(None, Some(format!("<p>{}</p>", "x".repeat(5000)))).unwrap(),
        );

        let err = transport.prepare(&email).unwrap_err();

        assert_eq!(err.kind, TransportErrorKind::PermanentMessage);
    }

    fn plain_email() -> catapulte_domain::port::email_sender::OutboundEmail {
        catapulte_domain::port::email_sender::OutboundEmail {
            sender: "from@example.com".into(),
            subject: Some("hi".into()),
            recipients: vec![(RecipientKind::To, "to@example.com".into())],
            body: plain_text_body(),
            attachments: vec![],
            reply_to: vec![],
            headers: vec![],
            message_id: "<018f4e3c-2d1a-7b3c-8f00-1234567890ab@example.com>".into(),
        }
    }

    #[tokio::test]
    async fn reply_to_and_custom_headers_appear_in_the_message() {
        let transport = local_transport(None);
        let mut email = plain_email();
        email.reply_to = vec![
            "Support <help@example.com>".into(),
            "ops@example.com".into(),
        ];
        email.headers = vec![
            ("X-Order-Id".into(), "42".into()),
            ("In-Reply-To".into(), "<a@b.c>".into()),
        ];

        let (_, raw) = transport.prepare(&email).unwrap();

        let text = String::from_utf8_lossy(&raw);
        assert!(
            text.contains("Reply-To: Support <help@example.com>, ops@example.com"),
            "{text}"
        );
        assert!(text.contains("X-Order-Id: 42"), "{text}");
        assert!(text.contains("In-Reply-To: <a@b.c>"), "{text}");
    }

    #[tokio::test]
    async fn non_ascii_header_value_is_encoded_not_sent_raw() {
        let transport = local_transport(None);
        let mut email = plain_email();
        email.headers = vec![("X-Note".into(), "caf\u{e9}".into())];

        let (_, raw) = transport.prepare(&email).unwrap();

        assert!(
            String::from_utf8_lossy(&raw)
                .to_ascii_lowercase()
                .contains("x-note: =?utf-8?"),
            "non-ASCII value must be RFC 2047 encoded"
        );
    }

    #[tokio::test]
    async fn invalid_header_name_or_reply_to_is_a_message_level_failure() {
        let transport = local_transport(None);
        let mut bad_name = plain_email();
        bad_name.headers = vec![("Bad Name".into(), "v".into())];
        let mut bad_reply_to = plain_email();
        bad_reply_to.reply_to = vec!["not an address".into()];

        for email in [bad_name, bad_reply_to] {
            let err = transport.prepare(&email).unwrap_err();
            assert_eq!(err.kind, TransportErrorKind::PermanentMessage);
        }
    }

    #[tokio::test]
    async fn header_line_limit_is_enforced_on_the_final_message() {
        let transport = local_transport(None);
        let with_value = |n: usize| {
            let mut email = plain_email();
            email.headers = vec![("X-A".into(), "v".repeat(n))];
            email
        };

        assert!(transport.prepare(&with_value(993)).is_ok());
        let err = transport.prepare(&with_value(994)).unwrap_err();
        assert_eq!(err.kind, TransportErrorKind::PermanentMessage);

        let mut long_name = plain_email();
        long_name.headers = vec![("N".repeat(76), "v".repeat(920))];
        assert!(transport.prepare(&long_name).is_ok());
        long_name.headers = vec![("N".repeat(76), "v".repeat(921))];
        assert!(transport.prepare(&long_name).is_err());
    }

    fn header_block(raw: &[u8]) -> &[u8] {
        let end = raw
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .expect("header block end");
        &raw[..end]
    }

    #[tokio::test]
    async fn space_separated_long_value_is_folded_within_the_line_limit() {
        let transport = local_transport(None);
        let mut email = plain_email();
        // 76 + 2 + 949 characters would be 1027 on one line.
        email.headers = vec![("N".repeat(76), "word ".repeat(190).trim_end().to_owned())];

        let (_, raw) = transport.prepare(&email).unwrap();

        let headers = header_block(&raw);
        let text = String::from_utf8_lossy(headers);
        assert!(
            text.contains("\r\n\t") || text.contains("\r\n "),
            "no fold: {text}"
        );
        assert!(
            headers
                .split(|b| *b == b'\n')
                .all(|l| l.strip_suffix(b"\r").unwrap_or(l).len() <= 998)
        );
    }

    #[tokio::test]
    async fn tab_separated_long_value_is_not_folded_and_fails() {
        let transport = local_transport(None);
        let mut email = plain_email();
        email.headers = vec![("N".repeat(76), "word\t".repeat(190))];

        assert!(transport.prepare(&email).is_err());
    }

    #[tokio::test]
    async fn over_long_subject_and_names_fail_while_just_under_builds() {
        let transport = local_transport(None);
        for (label, long, under) in [
            ("subject", "s".repeat(1000), "s".repeat(900)),
            ("sender", "n".repeat(1000), "n".repeat(900)),
            ("recipient", "n".repeat(1000), "n".repeat(900)),
        ] {
            let build = |v: &str| {
                let mut email = plain_email();
                match label {
                    "subject" => email.subject = Some(v.to_owned()),
                    "sender" => email.sender = format!("{v} <from@example.com>"),
                    _ => {
                        email.recipients =
                            vec![(RecipientKind::To, format!("{v} <to@example.com>"))];
                    }
                }
                transport.prepare(&email)
            };
            assert!(build(&under).is_ok(), "{label} under the limit");
            assert_eq!(
                build(&long).unwrap_err().kind,
                TransportErrorKind::PermanentMessage,
                "{label} over the limit"
            );
        }
    }

    #[tokio::test]
    async fn long_body_line_and_long_mime_part_header_do_not_trip_the_top_level_check() {
        let transport = local_transport(None);
        let mut email = plain_email();
        email.body = RenderedBody::new(Plain::try_new(Some("b".repeat(5000)), None).unwrap());
        email.attachments = vec![ResolvedAttachment {
            filename: "f".repeat(1200),
            content_type: "text/plain".to_owned(),
            bytes: bytes::Bytes::from_static(b"data"),
            content_id: None,
        }];

        let (_, raw) = transport.prepare(&email).unwrap();

        let top_ok = header_block(&raw)
            .split(|b| *b == b'\n')
            .all(|l| l.strip_suffix(b"\r").unwrap_or(l).len() <= 998);
        assert!(top_ok);
    }

    #[test]
    fn line_check_looks_only_at_the_top_level_header_block() {
        let long = "x".repeat(2000);
        let body_long = format!("Subject: hi\r\n\r\n{long}\r\n");
        let part_header_long = format!("Subject: hi\r\n\r\n--b\r\nContent-Disposition: {long}\r\n");
        let header_long = format!("Subject: {long}\r\n\r\nbody");
        assert!(super::check_header_line_lengths(body_long.as_bytes()).is_ok());
        assert!(super::check_header_line_lengths(part_header_long.as_bytes()).is_ok());
        assert!(super::check_header_line_lengths(header_long.as_bytes()).is_err());
    }

    #[tokio::test]
    async fn message_id_header_is_set_from_the_outbound_email() {
        let transport = local_transport(None);
        let email = plain_email();

        let (_, raw) = transport.prepare(&email).unwrap();

        let text = String::from_utf8_lossy(&raw);
        assert!(
            text.contains("Message-ID: <018f4e3c-2d1a-7b3c-8f00-1234567890ab@example.com>"),
            "{text}"
        );
    }

    #[tokio::test]
    async fn message_id_survives_the_header_line_check_with_a_long_domain() {
        let transport = local_transport(None);
        let mut email = plain_email();
        email.message_id = format!(
            "<{}@{}.example.com>",
            "00000000-0000-0000-0000-000000000000",
            "d".repeat(60)
        );

        assert!(transport.prepare(&email).is_ok());
    }

    #[tokio::test]
    async fn punycode_message_id_is_written_verbatim() {
        let transport = local_transport(None);
        let mut email = plain_email();
        email.message_id = "<018f4e3c-2d1a-7b3c-8f00-1234567890ab@xn--bcher-kva.de>".into();

        let (_, raw) = transport.prepare(&email).unwrap();

        let text = String::from_utf8_lossy(&raw);
        assert!(
            text.contains("Message-ID: <018f4e3c-2d1a-7b3c-8f00-1234567890ab@xn--bcher-kva.de>"),
            "{text}"
        );
    }

    #[test]
    fn smtp_response_is_code_and_joined_lines_and_capped() {
        use super::format_smtp_response;
        assert_eq!(
            format_smtp_response(250, ["2.0.0 Ok: queued as 4ABC", "second"].into_iter()),
            "250 2.0.0 Ok: queued as 4ABC second"
        );
        let long = "x".repeat(1000);
        let capped = format_smtp_response(250, std::iter::once(long.as_str()));
        assert_eq!(capped.chars().count(), 256);
        assert!(capped.starts_with("250 "));
    }
}
