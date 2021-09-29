use crate::config::Config;
use jsonwebtoken::{decode, errors::Error as JwtError, Algorithm, DecodingKey, Validation};
use std::str::FromStr;
use std::sync::Arc;

const DEFAULT_SECRET: &str = "I LOVE CATAPULTE";

#[derive(Debug, serde::Deserialize)]
#[cfg_attr(test, derive(serde::Serialize))]
pub struct Claims {
    exp: usize,
}

#[derive(Clone, Debug)]
pub struct Decoder {
    key: DecodingKey<'static>,
    validation: Validation,
}

impl From<Arc<Config>> for Decoder {
    fn from(root: Arc<Config>) -> Self {
        Self {
            key: Self::parse_decoding_key(root.clone()),
            validation: Validation::new(Self::parse_algorithm(root)),
        }
    }
}

impl Decoder {
    fn parse_algorithm(root: Arc<Config>) -> Algorithm {
        root.jwt_algorithm
            .as_ref()
            .map(|value| Algorithm::from_str(value).expect("unable to parse jwt algorithm"))
            .unwrap_or_default()
    }

    fn parse_decoding_key(root: Arc<Config>) -> DecodingKey<'static> {
        if let Some(ref secret) = root.jwt_secret {
            DecodingKey::from_secret(secret.as_bytes()).into_static()
        } else if let Some(ref secret) = root.jwt_secret_base64 {
            DecodingKey::from_base64_secret(secret.as_str()).expect("couldn't decode base64 secret")
        } else if let Some(ref key) = root.jwt_rsa_pem {
            DecodingKey::from_rsa_pem(key.as_bytes())
                .map(|value| value.into_static())
                .expect("couldn't read rsa pem")
        } else if let Some(ref key) = root.jwt_ec_pem {
            DecodingKey::from_ec_pem(key.as_bytes())
                .map(|value| value.into_static())
                .expect("couldn't read ec pem")
        } else if let Some(ref der) = root.jwt_rsa_der {
            DecodingKey::from_rsa_der(der.as_bytes()).into_static()
        } else if let Some(ref der) = root.jwt_ec_der {
            DecodingKey::from_ec_der(der.as_bytes()).into_static()
        } else {
            log::warn!(
                "no JWT decoding key provided, using the default \"{}\"",
                DEFAULT_SECRET
            );
            DecodingKey::from_secret(DEFAULT_SECRET.as_bytes()).into_static()
        }
    }

    pub fn decode(&self, token: &str) -> Result<Claims, JwtError> {
        decode::<Claims>(token, &self.key, &self.validation).map(|result| result.claims)
    }
}

// LCOV_EXCL_START
#[cfg(test)]
pub mod tests {
    use super::Decoder;
    use crate::config::Config;
    use std::time::{Duration, SystemTime};

    pub fn create_token() -> String {
        let now = SystemTime::now()
            .checked_add(Duration::from_secs(60))
            .unwrap();
        let payload = super::Claims {
            exp: now
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_secs() as usize,
        };
        jsonwebtoken::encode(
            &jsonwebtoken::Header::default(),
            &payload,
            &jsonwebtoken::EncodingKey::from_secret(super::DEFAULT_SECRET.as_ref()),
        )
        .unwrap()
        .to_string()
    }

    #[test]
    fn parse_and_decode() {
        let token = create_token();
        let cfg = Config::from_args(vec![]);
        let result = super::Decoder::from(cfg).decode(&token);
        assert!(result.is_ok());
        let token = create_token();
        let cfg = Config::from_args(vec!["--jwt-secret".to_string(), token.clone()]);
        let result = Decoder::from(cfg.clone()).decode(&token);
        assert!(result.is_err());
        let result = Decoder::from(cfg).decode("abcd");
        assert!(result.is_err());
    }

    #[test]
    fn algorithm_parsing() {
        let cfg = Config::from_args(vec![]);
        assert_eq!(Decoder::parse_algorithm(cfg), super::Algorithm::default());
        let cfg = Config::from_args(vec!["--jwt-algorithm=HS384".to_string()]);
        assert_eq!(Decoder::parse_algorithm(cfg), super::Algorithm::HS384);
    }

    #[test]
    fn decoding_key_parsing_empty() {
        let cfg = Config::from_args(vec![]);
        let _ = Decoder::parse_decoding_key(cfg);
    }

    #[test]
    fn decoding_key_parsing_secret() {
        let cfg = Config::from_args(vec!["--jwt-secret".to_string(), "qwertyuiop".to_string()]);
        let _ = Decoder::parse_decoding_key(cfg);
    }

    #[test]
    fn decoding_key_parsing_secret_base64() {
        let cfg = Config::from_args(vec![
            "--jwt-secret-base64".to_string(),
            "0123456789ABCDEF".to_string(),
        ]);
        let _ = Decoder::parse_decoding_key(cfg);
    }
}
// LCOV_EXCL_END
