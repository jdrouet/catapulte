use anyhow::Context;
use catapulte_domain::entity::attachment::ResolvedAttachment;
use catapulte_domain::entity::body::RenderedBody;
use catapulte_domain::entity::email::RecipientKind;
use catapulte_domain::port::email_sender::OutboundEmail;
use catapulte_domain::port::email_transport::EmailTransport;
use lettre::message::header::{ContentDisposition, ContentType};
use lettre::message::{Mailbox, MultiPart, SinglePart};
use lettre::transport::smtp::authentication::Credentials;
use lettre::{Address, AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};

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

fn parse_mailbox(addr: &str) -> anyhow::Result<Mailbox> {
    addr.parse::<Address>()
        .with_context(|| format!("invalid address: {addr}"))
        .map(|a| Mailbox::new(None, a))
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
        Ok(SmtpTransport { transport })
    }
}

pub struct SmtpTransport {
    transport: AsyncSmtpTransport<Tokio1Executor>,
}

impl SmtpTransport {
    pub(crate) async fn send_inner(&self, email: &OutboundEmail) -> anyhow::Result<()> {
        let from = parse_mailbox(&email.sender)?;
        let builder = apply_recipients(Message::builder().from(from), &email.recipients)?;
        let message = finalize_message(
            builder,
            email.subject.as_deref(),
            &email.body,
            &email.attachments,
        )?;
        self.transport
            .send(message)
            .await
            .context("smtp send failed")?;
        Ok(())
    }
}

impl EmailTransport for SmtpTransport {
    async fn deliver<'a>(&'a self, email: &'a OutboundEmail) -> Result<(), anyhow::Error> {
        self.send_inner(email).await
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::env::VarError;

    use catapulte_domain::entity::attachment::ResolvedAttachment;
    use catapulte_domain::entity::body::{Plain, RenderedBody};
    use lettre::Address;

    use super::{SmtpConfig, SmtpTls, finalize_message, parse_port, parse_tls};
    use crate::transport::parse_mailbox;

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
}
