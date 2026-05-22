use anyhow::Context;
use catapulte_domain::entity::body::RenderedBody;
use catapulte_domain::entity::email::RecipientKind;
use catapulte_domain::port::email_sender::{EmailSender, SendError};
use lettre::message::header::ContentType;
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

impl SmtpConfig {
    /// # Errors
    ///
    /// Returns an error if a required environment variable is missing or has an invalid value.
    pub fn from_env(prefix: &str) -> anyhow::Result<Self> {
        Self::from_lookup(prefix, |key| std::env::var(key))
    }

    fn from_lookup<F>(prefix: &str, lookup: F) -> anyhow::Result<Self>
    where
        F: Fn(&str) -> Result<String, std::env::VarError>,
    {
        let host_key = format!("{prefix}_HOST");
        let host = lookup(&host_key).with_context(|| format!("missing env var {host_key}"))?;

        let port_key = format!("{prefix}_PORT");
        let port = match lookup(&port_key) {
            Ok(val) => val
                .parse::<u16>()
                .with_context(|| format!("invalid value for env var {port_key}"))?,
            Err(_) => 587,
        };

        let username_key = format!("{prefix}_USERNAME");
        let username = lookup(&username_key).ok();

        let password_key = format!("{prefix}_PASSWORD");
        let password = lookup(&password_key).ok();

        let tls_key = format!("{prefix}_TLS");
        let tls = match lookup(&tls_key) {
            Ok(val) => match val.as_str() {
                "starttls" => SmtpTls::Starttls,
                "tls" => SmtpTls::Tls,
                "none" => SmtpTls::None,
                other => {
                    anyhow::bail!("unknown value for env var {tls_key}: {other}")
                }
            },
            Err(_) => SmtpTls::Starttls,
        };

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
    pub fn build(self) -> anyhow::Result<SmtpSender> {
        let builder = match self.tls {
            SmtpTls::Starttls => AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&self.host)
                .context("building smtp transport")?,
            SmtpTls::Tls => AsyncSmtpTransport::<Tokio1Executor>::relay(&self.host)
                .context("building smtp transport")?,
            SmtpTls::None => AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&self.host),
        };

        let builder = builder.port(self.port);

        let builder = if let (Some(username), Some(password)) = (self.username, self.password) {
            builder.credentials(Credentials::new(username, password))
        } else {
            builder
        };

        let transport = builder.build();

        Ok(SmtpSender { transport })
    }
}

pub struct SmtpSender {
    transport: AsyncSmtpTransport<Tokio1Executor>,
}

impl EmailSender for SmtpSender {
    async fn send(
        &self,
        sender: &str,
        recipients: &[(RecipientKind, String)],
        body: &RenderedBody,
    ) -> Result<(), SendError> {
        let sender_addr = sender
            .parse::<Address>()
            .context("invalid sender address")
            .map_err(|source| SendError::Send { source })?;

        let mut builder = Message::builder().from(Mailbox::new(Option::None, sender_addr));

        for (kind, address) in recipients {
            let addr = address
                .parse::<Address>()
                .with_context(|| format!("invalid recipient address: {address}"))
                .map_err(|source| SendError::Send { source })?;
            let mailbox = Mailbox::new(Option::None, addr);
            builder = match kind {
                RecipientKind::To => builder.to(mailbox),
                RecipientKind::Cc => builder.cc(mailbox),
                RecipientKind::Bcc => builder.bcc(mailbox),
            };
        }

        let message = match (body.text(), body.html()) {
            (Some(text), Some(html)) => {
                let text_part = SinglePart::builder()
                    .header(ContentType::TEXT_PLAIN)
                    .body(text.to_owned());
                let html_part = SinglePart::builder()
                    .header(ContentType::TEXT_HTML)
                    .body(html.to_owned());
                builder
                    .multipart(
                        MultiPart::alternative()
                            .singlepart(text_part)
                            .singlepart(html_part),
                    )
                    .context("building email message")
                    .map_err(|source| SendError::Send { source })?
            }
            (Some(text), Option::None) => builder
                .singlepart(
                    SinglePart::builder()
                        .header(ContentType::TEXT_PLAIN)
                        .body(text.to_owned()),
                )
                .context("building email message")
                .map_err(|source| SendError::Send { source })?,
            (Option::None, Some(html)) => builder
                .singlepart(
                    SinglePart::builder()
                        .header(ContentType::TEXT_HTML)
                        .body(html.to_owned()),
                )
                .context("building email message")
                .map_err(|source| SendError::Send { source })?,
            (Option::None, Option::None) => {
                unreachable!("Plain invariant: at least one of text or html must be provided")
            }
        };

        self.transport
            .send(message)
            .await
            .context("smtp send failed")
            .map_err(|source| SendError::Send { source })?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::env::VarError;

    use super::{SmtpConfig, SmtpTls};

    fn make_lookup(
        vars: HashMap<&'static str, &'static str>,
    ) -> impl Fn(&str) -> Result<String, VarError> {
        move |key: &str| {
            vars.get(key)
                .map(|v| (*v).to_owned())
                .ok_or(VarError::NotPresent)
        }
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
}
