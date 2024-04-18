use lettre::transport::smtp::authentication::Credentials;
use lettre::transport::smtp::client::{Tls, TlsParameters};
use lettre::transport::smtp::{
    Error as LettreError, PoolConfig, SmtpTransport, SmtpTransportBuilder,
};
use std::time::Duration;

pub type SmtpPool = SmtpTransport;

#[derive(Clone, Debug, serde::Deserialize)]
pub struct Configuration {
    #[serde(default = "Configuration::default_hostname")]
    pub hostname: String,
    #[serde(default = "Configuration::default_port")]
    pub port: u16,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default = "Configuration::default_max_pool_size")]
    pub max_pool_size: u32,
    #[serde(default)]
    pub tls_enabled: bool,
    #[serde(default = "Configuration::default_timeout")]
    pub timeout: u64,
    #[serde(default)]
    pub accept_invalid_cert: bool,
}

impl Default for Configuration {
    fn default() -> Self {
        Self {
            hostname: Self::default_hostname(),
            port: Self::default_port(),
            username: None,
            password: None,
            max_pool_size: Self::default_max_pool_size(),
            tls_enabled: false,
            timeout: Self::default_timeout(),
            accept_invalid_cert: false,
        }
    }
}

impl Configuration {
    fn default_hostname() -> String {
        "127.0.0.1".into()
    }

    fn default_port() -> u16 {
        25
    }

    fn default_max_pool_size() -> u32 {
        10
    }

    fn default_timeout() -> u64 {
        5000
    }
}

#[cfg(test)]
impl Configuration {
    pub(crate) fn insecure(port: u16) -> Self {
        Self {
            hostname: "localhost".to_string(),
            port,
            username: None,
            password: None,
            max_pool_size: Self::default_max_pool_size(),
            tls_enabled: false,
            timeout: Self::default_timeout(),
            accept_invalid_cert: false,
        }
    }

    // pub(crate) fn secure(port: u16) -> Self {
    //     Self {
    //         hostname: tests::env_str("TEST_SMTPS_HOSTNAME")
    //             .unwrap_or_else(|| "localhost".to_string()),
    //         port,
    //         username: None,
    //         password: None,
    //         max_pool_size: Self::default_max_pool_size(),
    //         tls_enabled: true,
    //         timeout: Self::default_timeout(),
    //         accept_invalid_cert: true,
    //     }
    // }
}

impl Configuration {
    fn get_credentials(&self) -> Option<Credentials> {
        if let Some(username) = self.username.as_ref() {
            tracing::debug!("using username {username:?}");
            if let Some(password) = self.password.as_ref() {
                Some(Credentials::new(username.clone(), password.clone()))
            } else {
                Some(Credentials::new(username.clone(), String::new()))
            }
        } else {
            tracing::debug!("using no username");
            None
        }
    }

    fn get_pool_config(&self) -> PoolConfig {
        tracing::debug!("with max pool size {}", self.max_pool_size);
        PoolConfig::default().max_size(self.max_pool_size)
    }

    fn get_timeout(&self) -> Duration {
        tracing::debug!("with timeout {}ms", self.timeout);
        Duration::from_millis(self.timeout)
    }

    // TODO allow to add root certificate
    // TODO allow to accept invalid hostnames
    fn get_tls(&self) -> Result<Tls, ConfigurationError> {
        if self.tls_enabled {
            tracing::debug!("with tls enabled");
            let parameteres = TlsParameters::builder(self.hostname.to_string())
                .dangerous_accept_invalid_certs(self.accept_invalid_cert)
                .build_rustls()?;
            Ok(Tls::Required(parameteres))
        } else {
            tracing::debug!("with tls disabled");
            Ok(Tls::None)
        }
    }

    // TODO allow to specify authentication mechanism
    fn get_transport(&self) -> Result<SmtpTransportBuilder, ConfigurationError> {
        tracing::debug!(
            "connecting to hostname {:?} on port {}",
            self.hostname,
            self.port
        );
        let result = SmtpTransport::builder_dangerous(self.hostname.as_str())
            .port(self.port)
            .timeout(Some(self.get_timeout()))
            .pool_config(self.get_pool_config())
            .tls(self.get_tls()?);
        let result = if let Some(creds) = self.get_credentials() {
            result.credentials(creds)
        } else {
            result
        };
        Ok(result)
    }

    pub(crate) fn build(&self) -> Result<SmtpTransport, ConfigurationError> {
        tracing::debug!("building smtp pool");
        let mailer = self.get_transport()?;
        Ok(mailer.build())
    }
}

#[derive(Debug)]
pub(crate) struct ConfigurationError(#[allow(dead_code)] LettreError);

impl From<LettreError> for ConfigurationError {
    fn from(err: LettreError) -> Self {
        tracing::error!("smtp configuration error: {:?}", err);
        Self(err)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::Arc;

    use lettre::{
        message::{Mailbox, Mailboxes},
        Address,
    };
    use serde::Deserialize;
    use testcontainers::{core::WaitFor, GenericImage};
    use uuid::Uuid;

    pub fn smtp_image_insecure() -> GenericImage {
        GenericImage::new("rnwood/smtp4dev", "v3")
            .with_wait_for(WaitFor::message_on_stdout(
                "Application started. Press Ctrl+C to shut down.",
            ))
            .with_env_var("ServerOptions__BasePath", "/")
            .with_env_var("ServerOptions__TlsMode", "None")
            .with_exposed_port(25)
            .with_exposed_port(80)
    }

    #[derive(Debug)]
    struct SmtpClient {
        host: String,
        port: u16,
    }

    impl SmtpClient {
        async fn query_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> T {
            reqwest::get(format!("http://{}:{}{path}", self.host, self.port))
                .await
                .unwrap()
                .json::<T>()
                .await
                .unwrap()
        }

        async fn query_text(&self, path: &str) -> String {
            reqwest::get(format!("http://{}:{}{path}", self.host, self.port))
                .await
                .unwrap()
                .text()
                .await
                .unwrap()
        }
    }

    pub struct SmtpMock {
        client: Arc<SmtpClient>,
    }

    impl SmtpMock {
        pub fn new(host: impl Into<String>, port: u16) -> Self {
            Self {
                client: Arc::new(SmtpClient {
                    host: host.into(),
                    port,
                }),
            }
        }

        pub async fn latest_inbox(&self) -> Vec<AbstractEmail> {
            self.client.query_json("/api/Messages").await
        }

        pub async fn expect_latest_inbox(&self) -> Vec<Wrapped<AbstractEmail>> {
            for _ in 0..10 {
                let list = self.latest_inbox().await;
                if !list.is_empty() {
                    return list
                        .into_iter()
                        .map(|inner| Wrapped {
                            client: self.client.clone(),
                            inner,
                        })
                        .collect();
                }
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            }
            panic!("mailbox is empty");
        }
    }

    #[derive(Debug)]
    pub struct Wrapped<I> {
        client: Arc<SmtpClient>,
        pub inner: I,
    }

    #[derive(Debug, Deserialize)]
    pub(crate) struct AbstractEmail {
        pub id: String,
        pub from: String,
        pub to: Mailboxes,
        pub subject: String,
    }

    impl Wrapped<AbstractEmail> {
        pub async fn detailed(&self) -> Wrapped<Email> {
            let path = format!("/api/Messages/{}", self.inner.id);
            Wrapped {
                client: self.client.clone(),
                inner: self.client.query_json(&path).await,
            }
        }
    }

    #[derive(Debug, Deserialize)]
    pub struct Email {
        pub id: String,
        pub from: Mailbox,
        pub to: Mailboxes,
        pub cc: Mailboxes,
        pub bcc: Mailboxes,
        pub subject: String,
        pub parts: Vec<EmailPart>,
        pub headers: Vec<EmailHeader>,
    }

    impl Wrapped<Email> {
        async fn child_part_source_with_content_type(&self, content_type: &str) -> Option<String> {
            let part = self.inner.parts.iter().find_map(|part| {
                part.child_parts.iter().find_map(|child| {
                    if child
                        .headers
                        .iter()
                        .find(|h| h.name == "Content-Type" && h.value.starts_with(content_type))
                        .is_some()
                    {
                        Some(child)
                    } else {
                        None
                    }
                })
            })?;
            let path = format!("/api/Messages/{}/part/{}/source", self.inner.id, part.id);
            Some(self.client.query_text(&path).await)
        }

        pub async fn plaintext(&self) -> String {
            self.child_part_source_with_content_type("text/plain")
                .await
                .unwrap_or_default()
        }

        pub async fn html(&self) -> String {
            self.child_part_source_with_content_type("text/html")
                .await
                .unwrap_or_default()
        }
    }

    #[derive(Debug, Deserialize)]
    pub struct EmailHeader {
        pub name: String,
        pub value: String,
    }

    #[derive(Debug, Deserialize)]
    pub struct EmailPart {
        pub id: String,
        pub name: String,
        pub headers: Vec<EmailHeader>,
        #[serde(alias = "childParts")]
        pub child_parts: Vec<EmailPart>,
    }

    pub(crate) fn create_email() -> Mailbox {
        Mailbox::new(
            None,
            Address::new(Uuid::new_v4().to_string(), "example.org").unwrap(),
        )
    }
}
