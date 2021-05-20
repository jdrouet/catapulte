use jsonwebtoken::{decode, errors::Error as JwtError, Algorithm, DecodingKey, Validation};
use std::env;
use std::str::FromStr;

#[derive(Debug)]
pub enum ParserError {
    Jwt(JwtError),
    Algorithm(String),
}

impl From<JwtError> for ParserError {
    fn from(err: JwtError) -> Self {
        Self::Jwt(err)
    }
}

fn parse_algorithm() -> Result<Algorithm, ParserError> {
    if let Ok(value) = env::var("JWT_ALGORITHM") {
        Algorithm::from_str(&value).map_err(|err| ParserError::Algorithm(err.to_string()))
    } else {
        Ok(Algorithm::default())
    }
}

fn parse_decoding_key() -> Result<Option<DecodingKey<'static>>, ParserError> {
    if let Ok(secret) = env::var("JWT_SECRET") {
        Ok(Some(
            DecodingKey::from_secret(secret.as_bytes()).into_static(),
        ))
    } else if let Ok(secret) = env::var("JWT_SECRET_BASE64") {
        Ok(DecodingKey::from_base64_secret(secret.as_str()).map(Some)?)
    } else if let Ok(key) = env::var("JWT_RSA_PEM") {
        Ok(DecodingKey::from_rsa_pem(key.as_bytes()).map(|value| Some(value.into_static()))?)
    } else if let Ok(key) = env::var("JWT_EC_PEM") {
        Ok(DecodingKey::from_ec_pem(key.as_bytes()).map(|value| Some(value.into_static()))?)
    } else if let Ok(der) = env::var("JWT_RSA_DER") {
        Ok(Some(
            DecodingKey::from_rsa_der(der.as_bytes()).into_static(),
        ))
    } else if let Ok(der) = env::var("JWT_EC_DER") {
        Ok(Some(DecodingKey::from_ec_der(der.as_bytes()).into_static()))
    } else {
        Ok(None)
    }
}

#[derive(Debug)]
pub enum DecoderError {
    Jwt(JwtError),
    TokenNotFound,
}

#[derive(Debug, serde::Deserialize)]
#[cfg_attr(test, derive(serde::Serialize))]
pub struct Claims {
    exp: usize,
}

#[derive(Clone, Debug)]
pub struct Decoder {
    key: Option<DecodingKey<'static>>,
    validation: Validation,
}

impl Decoder {
    pub fn from_env() -> Result<Self, ParserError> {
        Ok(Self {
            key: parse_decoding_key()?,
            validation: Validation::new(parse_algorithm()?),
        })
    }

    pub fn decode(&self, token: Option<&str>) -> Result<Option<Claims>, DecoderError> {
        if let Some(ref key) = self.key {
            if let Some(token) = token {
                decode::<Claims>(token, &key, &self.validation)
                    .map(|result| Some(result.claims))
                    .map_err(DecoderError::Jwt)
            } else {
                Err(DecoderError::TokenNotFound)
            }
        } else {
            Ok(None)
        }
    }
}

// LCOV_EXCL_START
#[cfg(test)]
pub mod tests {
    use super::parse_algorithm;
    use super::parse_decoding_key;
    use env_test_util::TempEnvVar;
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
            &jsonwebtoken::EncodingKey::from_secret("secret".as_ref()),
        )
        .unwrap()
        .to_string()
    }

    #[test]
    #[serial]
    fn parse_and_decode() {
        let token = create_token();
        let _value = TempEnvVar::new("JWT_ALGORITHM");
        let _secret = TempEnvVar::new("JWT_SECRET");
        let result = super::Decoder::from_env().unwrap().decode(None);
        assert!(result.unwrap().is_none());
        let _secret = TempEnvVar::new("JWT_SECRET").with("secret");
        let result = super::Decoder::from_env().unwrap().decode(Some(&token));
        assert!(result.unwrap().is_some());
        let result = super::Decoder::from_env().unwrap().decode(Some("abcd"));
        assert!(result.is_err());
        let result = super::Decoder::from_env().unwrap().decode(None);
        assert!(result.is_err());
    }

    #[test]
    #[serial]
    fn algorithm_parsing() {
        let _value = TempEnvVar::new("JWT_ALGORITHM");
        assert_eq!(parse_algorithm().unwrap(), super::Algorithm::default());
        let _value = TempEnvVar::new("JWT_ALGORITHM").with("HS384");
        assert_eq!(parse_algorithm().unwrap(), super::Algorithm::HS384);
    }

    #[test]
    #[serial]
    fn decoding_key_parsing_empty() {
        let _secret = TempEnvVar::new("JWT_SECRET");
        let _secret_base64 = TempEnvVar::new("JWT_SECRET_BASE64");
        let _ec_pem = TempEnvVar::new("JWT_EC_PEM");
        let _ec_der = TempEnvVar::new("JWT_EC_DER");
        let _rsa_pem = TempEnvVar::new("JWT_RSA_PEM");
        let _rsa_der = TempEnvVar::new("JWT_RSA_DER");
        assert!(parse_decoding_key().unwrap().is_none());
    }

    #[test]
    #[serial]
    fn decoding_key_parsing_secret() {
        let _secret = TempEnvVar::new("JWT_SECRET").with("qwertyuiop");
        let _secret_base64 = TempEnvVar::new("JWT_SECRET_BASE64");
        let _ec_pem = TempEnvVar::new("JWT_EC_PEM");
        let _ec_der = TempEnvVar::new("JWT_EC_DER");
        let _rsa_pem = TempEnvVar::new("JWT_RSA_PEM");
        let _rsa_der = TempEnvVar::new("JWT_RSA_DER");
        assert!(parse_decoding_key().unwrap().is_some());
    }

    #[test]
    #[serial]
    fn decoding_key_parsing_secret_base64() {
        let _secret = TempEnvVar::new("JWT_SECRET");
        let _secret_base64 = TempEnvVar::new("JWT_SECRET_BASE64").with("0123456789ABCDEF");
        let _ec_pem = TempEnvVar::new("JWT_EC_PEM");
        let _ec_der = TempEnvVar::new("JWT_EC_DER");
        let _rsa_pem = TempEnvVar::new("JWT_RSA_PEM");
        let _rsa_der = TempEnvVar::new("JWT_RSA_DER");
        assert!(parse_decoding_key().unwrap().is_some());
    }
}
// LCOV_EXCL_END
