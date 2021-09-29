use clap::Clap;
use std::sync::Arc;

#[derive(Clap, Debug)]
pub struct Config {
    #[clap(long, env = "AUTHENTICATION_ENABLED")]
    pub authentication_enabled: bool,
    #[clap(long, env = "AUTHENTICATION_HEAD", default_value = "Authorization")]
    pub authentication_header: String,
    #[clap(long, env = "JWT_ALGORITHM")]
    pub jwt_algorithm: Option<String>,
    #[clap(long, env = "JWT_SECRET")]
    pub jwt_secret: Option<String>,
    #[clap(long, env = "JWT_SECRET_BASE64")]
    pub jwt_secret_base64: Option<String>,
    #[clap(long, env = "JWT_RSA_PEM")]
    pub jwt_rsa_pem: Option<String>,
    #[clap(long, env = "JWT_EC_PEM")]
    pub jwt_ec_pem: Option<String>,
    #[clap(long, env = "JWT_RSA_DER")]
    pub jwt_rsa_der: Option<String>,
    #[clap(long, env = "JWT_EC_DER")]
    pub jwt_ec_der: Option<String>,
    #[clap(long = "address", env = "ADDRESS", default_value = "127.0.0.1")]
    pub server_address: String,
    #[clap(long = "port", env = "PORT", default_value = "3000")]
    pub server_port: u16,
    #[clap(long, env = "SMTP_HOSTNAME", default_value = "127.0.0.1")]
    pub smtp_hostname: String,
    #[clap(long, env = "SMTP_PORT", default_value = "25")]
    pub smtp_port: u16,
    #[clap(long, env = "SMTP_USERNAME")]
    pub smtp_username: Option<String>,
    #[clap(long, env = "SMTP_PASSWORD")]
    pub smtp_password: Option<String>,
    #[clap(long, env = "SMTP_MAX_POOL_SIZE", default_value = "10")]
    pub smtp_max_pool_size: u32,
    #[clap(long, env = "SMTP_TLS_ENABLED")]
    pub smtp_tls_enabled: bool,
    #[clap(long, env = "SMTP_TIMEOUT", default_value = "5000")]
    pub smtp_timeout: u64,
    #[clap(long, env = "SMTP_ACCEPT_INVALID_CERT")]
    pub smtp_accept_invalid_cert: bool,
    #[clap(long, env = "SWAGGER_ENABLED")]
    pub swagger_enabled: bool,
}

impl Config {
    pub fn build() -> Arc<Self> {
        Arc::new(Self::parse())
    }

    #[cfg(test)]
    pub fn from_args(inputs: Vec<String>) -> Arc<Self> {
        let mut args = vec!["catapulte".to_string()];
        args.extend(inputs);
        let res = Self::parse_from(args);
        Arc::new(res)
    }
}
