use anyhow::Context;
use catapulte_domain::entity::attachment::ResolvedAttachment;
use catapulte_domain::entity::body::RenderedBody;
use catapulte_domain::entity::email::RecipientKind;
use catapulte_domain::port::email_sender::OutboundEmail;
use catapulte_domain::port::email_transport::{EmailTransport, TransportError, TransportErrorKind};
use lettre::message::header::{ContentDisposition, ContentType};
use lettre::message::{Mailbox, MultiPart, SinglePart};
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
        let mut mixed = MultiPart::mixed().multipart(body_part);
        for att in attachments {
            let content_type = ContentType::parse(&att.content_type).with_context(|| {
                format!("invalid attachment content-type: {}", att.content_type)
            })?;
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

/// Caps remote-controlled reply text before it reaches events and logs.
fn truncate_reply(text: &str) -> String {
    match text.char_indices().nth(MAX_REPLY_CHARS) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text.to_owned(),
    }
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
        let builder = apply_recipients(Message::builder().from(from), &email.recipients)
            .map_err(message_error)?;
        let message = finalize_message(
            builder,
            email.subject.as_deref(),
            &email.body,
            &email.attachments,
        )
        .map_err(message_error)?;
        let raw = message.formatted();
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

    pub(crate) async fn send_inner(&self, email: &OutboundEmail) -> Result<(), TransportError> {
        let (message, raw) = self.prepare(email)?;
        self.transport
            .send_raw(message.envelope(), &raw)
            .await
            .map_err(|e| smtp_text_error(e.is_permanent(), &e.to_string()))?;
        Ok(())
    }
}

impl EmailTransport for SmtpTransport {
    async fn deliver<'a>(&'a self, email: &'a OutboundEmail) -> Result<(), TransportError> {
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
        };
        let att2 = ResolvedAttachment {
            filename: "b.txt".to_owned(),
            content_type: "text/plain".to_owned(),
            bytes: bytes::Bytes::from_static(b"file b"),
        };
        assert!(finalize_message(base_builder(), Some("Multi"), &body, &[att1, att2]).is_ok());
    }

    #[test]
    fn finalize_message_with_invalid_content_type_returns_error() {
        let body = plain_text_body();
        let att = ResolvedAttachment {
            filename: "bad.bin".to_owned(),
            content_type: "not a content type///".to_owned(),
            bytes: bytes::Bytes::from_static(b"data"),
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
        assert_eq!(cut.chars().count(), 257);
        assert!(cut.ends_with('…'));
        assert!(cut.starts_with(&"é".repeat(256)));
    }

    #[test]
    fn classification_uses_the_full_text_before_truncation() {
        let text = format!("{}): 5.1.1 no such user", "x".repeat(300));

        let err = smtp_text_error(true, &text);

        assert_eq!(err.kind, TransportErrorKind::PermanentMessage);
        assert_eq!(err.source.to_string().chars().count(), 257);
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
                filename: "big.bin".into(),
                content_type: "application/octet-stream".into(),
                bytes: bytes::Bytes::from(vec![7u8; size]),
            }],
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
}
