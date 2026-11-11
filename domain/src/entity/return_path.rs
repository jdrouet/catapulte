use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::entity::email::EmailId;
use crate::use_case::submit_email::normalize_sender_domain;

/// Shortest accepted secret, in bytes.
const MIN_SECRET_BYTES: usize = 32;

/// Bytes of the HMAC kept in the address tag.
const TAG_BYTES: usize = 10;

/// Where bounces go and the key that signs the return paths pointing there.
#[derive(Clone)]
pub struct BounceConfig {
    domain: String,
    secret: Vec<u8>,
}

impl BounceConfig {
    /// # Errors
    ///
    /// Returns an error when the domain is not a valid hostname or the secret is shorter than
    /// 32 bytes.
    pub fn new(domain: &str, secret: &str) -> anyhow::Result<Self> {
        let domain = normalize_sender_domain(domain)
            .ok_or_else(|| anyhow::anyhow!("invalid bounce domain {domain:?}"))?;
        anyhow::ensure!(
            secret.len() >= MIN_SECRET_BYTES,
            "the bounce secret must be at least {MIN_SECRET_BYTES} bytes (for example `openssl rand -base64 48`)"
        );
        Ok(Self {
            domain,
            secret: secret.as_bytes().to_vec(),
        })
    }

    fn mac(&self, id: EmailId) -> Hmac<Sha256> {
        let mut mac =
            Hmac::<Sha256>::new_from_slice(&self.secret).expect("HMAC takes any key length");
        mac.update(id.as_uuid().as_bytes());
        mac
    }

    /// Lowercase unpadded base32 of the first 10 bytes of HMAC-SHA256 over the email id.
    #[must_use]
    pub fn sign(&self, id: EmailId) -> String {
        let digest = self.mac(id).finalize().into_bytes();
        data_encoding::BASE32_NOPAD
            .encode(&digest[..TAG_BYTES])
            .to_ascii_lowercase()
    }

    /// Checks a tag in constant time.
    #[must_use]
    pub fn verify(&self, id: EmailId, tag: &str) -> bool {
        let Ok(bytes) = data_encoding::BASE32_NOPAD.decode(tag.to_ascii_uppercase().as_bytes())
        else {
            return false;
        };
        bytes.len() == TAG_BYTES && self.mac(id).verify_truncated_left(&bytes).is_ok()
    }

    /// `bounces+{uuid}-{tag}@{domain}`.
    #[must_use]
    pub fn return_path(&self, id: EmailId) -> String {
        format!("bounces+{}-{}@{}", id.as_uuid(), self.sign(id), self.domain)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "0123456789abcdef0123456789abcdef";

    fn config() -> BounceConfig {
        BounceConfig::new("Bounces.Example.com", SECRET).unwrap()
    }

    /// Computed outside this crate with Python:
    /// `base64.b32encode(hmac.new(key, uuid.UUID(...).bytes, hashlib.sha256).digest()[:10])`,
    /// unpadded and lowercased.
    #[test]
    fn tag_matches_an_independently_computed_vector() {
        let id =
            EmailId::from(uuid::Uuid::parse_str("018f4e3c-2d1a-7b3c-8f00-1234567890ab").unwrap());

        assert_eq!(config().sign(id), "t6hpvmqrhhcbxr7n");
        assert!(config().verify(id, "t6hpvmqrhhcbxr7n"));
    }

    #[test]
    fn sign_and_verify_round_trip() {
        let id = EmailId::default();
        let tag = config().sign(id);

        assert_eq!(tag.len(), 16);
        assert!(
            tag.bytes()
                .all(|b| b.is_ascii_lowercase() || (b'2'..=b'7').contains(&b))
        );
        assert!(config().verify(id, &tag));
        assert_eq!(
            config().return_path(id),
            format!("bounces+{}-{tag}@bounces.example.com", id.as_uuid())
        );
    }

    #[test]
    fn tampered_wrong_key_wrong_id_and_malformed_tags_fail() {
        let id = EmailId::default();
        let tag = config().sign(id);
        let flipped = format!(
            "{}{}",
            &tag[..15],
            if tag.ends_with('a') { 'b' } else { 'a' }
        );

        assert!(!config().verify(id, &flipped));
        assert!(!config().verify(id, &tag[..15]));
        assert!(!config().verify(id, "not base32!"));
        assert!(!config().verify(EmailId::default(), &tag));
        let other = BounceConfig::new("bounces.example.com", "another-secret-of-at-least-32-bytes")
            .unwrap();
        assert!(!other.verify(id, &tag));
    }

    #[test]
    fn invalid_domain_or_empty_secret_is_rejected() {
        assert!(BounceConfig::new("not a domain", "s").is_err());
        assert!(BounceConfig::new("bounces.example.com", "").is_err());
        let err = BounceConfig::new("bounces.example.com", "short")
            .err()
            .unwrap();
        assert!(err.to_string().contains("at least 32 bytes"), "{err}");
        assert!(BounceConfig::new("bounces.example.com", &"k".repeat(32)).is_ok());
    }
}
