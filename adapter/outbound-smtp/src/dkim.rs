use std::env::VarError;

use anyhow::Context;
use lettre::message::dkim::{
    DkimCanonicalization, DkimCanonicalizationType, DkimConfig, DkimSigningAlgorithm,
    DkimSigningKey,
};
use lettre::message::header::HeaderName;

const SIGNED_HEADERS: &[&str] = &[
    "From",
    "Subject",
    "To",
    "Date",
    "Message-ID",
    "Reply-To",
    "List-Unsubscribe",
    "List-Unsubscribe-Post",
];

/// DKIM signing configurations keyed by lowercase signing domain.
#[derive(Default)]
pub struct DkimSigners {
    entries: Vec<(String, DkimConfig)>,
}

impl std::fmt::Debug for DkimSigners {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list()
            .entries(self.entries.iter().map(|(d, _)| d))
            .finish()
    }
}

impl DkimSigners {
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Reads `CATAPULTE_DKIM_DOMAINS` and, for each name, `CATAPULTE_DKIM_{NAME}_DOMAIN`,
    /// `_SELECTOR`, `_PRIVATE_KEY_FILE` and `_ALGORITHM` (`rsa` by default, or `ed25519`).
    ///
    /// # Errors
    ///
    /// Returns an error when a variable is missing or a key file cannot be read or parsed.
    pub(crate) fn from_lookup<F>(lookup: F) -> anyhow::Result<Self>
    where
        F: Fn(&str) -> Result<String, VarError>,
    {
        let names = match lookup("CATAPULTE_DKIM_DOMAINS") {
            Ok(names) => names,
            Err(VarError::NotPresent) => return Ok(Self::empty()),
            Err(e) => anyhow::bail!("invalid env var CATAPULTE_DKIM_DOMAINS: {e}"),
        };
        let mut entries: Vec<(String, DkimConfig)> = Vec::new();
        for name in names.split(',').map(str::trim).filter(|n| !n.is_empty()) {
            let prefix = format!("CATAPULTE_DKIM_{}", name.to_uppercase());
            let required = |suffix: &str| {
                let key = format!("{prefix}_{suffix}");
                lookup(&key).with_context(|| format!("missing env var {key}"))
            };
            let domain_raw = required("DOMAIN")?;
            let domain = normalize_domain(&domain_raw)
                .with_context(|| format!("invalid value for env var {prefix}_DOMAIN"))?;
            let selector = required("SELECTOR")?.trim().to_owned();
            validate_selector(&selector)
                .with_context(|| format!("invalid value for env var {prefix}_SELECTOR"))?;
            let key_file = required("PRIVATE_KEY_FILE")?;
            let algorithm = parse_algorithm(
                lookup(&format!("{prefix}_ALGORITHM")).ok().as_deref(),
                &format!("{prefix}_ALGORITHM"),
            )?;
            anyhow::ensure!(
                !entries.iter().any(|(d, _)| *d == domain),
                "DKIM domain {domain} is configured twice"
            );
            let pem = std::fs::read_to_string(&key_file)
                .with_context(|| format!("reading {prefix}_PRIVATE_KEY_FILE"))?;
            let config = build_config(&domain, selector, pem.trim(), algorithm)
                .with_context(|| format!("loading DKIM key for {prefix}"))?;
            entries.push((domain, config));
        }
        if !entries.is_empty() {
            let domains: Vec<&str> = entries.iter().map(|(d, _)| d.as_str()).collect();
            tracing::info!(count = domains.len(), ?domains, "DKIM signing configured");
        }
        Ok(Self { entries })
    }

    /// Picks the entry for the exact domain, else the longest configured parent domain.
    /// `domain` must already be normalized (see `normalize_domain`); matching is only at a
    /// label boundary.
    #[must_use]
    pub fn for_sender(&self, domain: &str) -> Option<&DkimConfig> {
        self.matching(domain).map(|(_, config)| config)
    }

    fn matching(&self, domain: &str) -> Option<&(String, DkimConfig)> {
        self.entries
            .iter()
            .filter(|(d, _)| domain == *d || domain.ends_with(&format!(".{d}")))
            .max_by_key(|(d, _)| d.len())
    }
}

/// Lowercase punycode form of a domain, accepted only as a plain LDH hostname.
pub(crate) fn normalize_domain(raw: &str) -> anyhow::Result<String> {
    let ascii = idna::domain_to_ascii(raw.trim())
        .map_err(|_| anyhow::anyhow!("not a valid domain name"))?
        .to_ascii_lowercase();
    anyhow::ensure!(
        !ascii.is_empty()
            && ascii.split('.').all(|label| {
                !label.is_empty()
                    && label
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            }),
        "domain must be dot-separated labels of letters, digits and '-'"
    );
    Ok(ascii)
}

/// RFC 6376 selector: dot-separated sub-domain labels of at most 63 characters.
fn validate_selector(selector: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        selector.split('.').all(|label| {
            (1..=63).contains(&label.len())
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        }),
        "selector must be dot-separated labels of letters, digits and '-', each at most 63 characters"
    );
    Ok(())
}

fn parse_algorithm(raw: Option<&str>, key: &str) -> anyhow::Result<DkimSigningAlgorithm> {
    match raw.map(|v| v.trim().to_ascii_lowercase()).as_deref() {
        None | Some("rsa") => Ok(DkimSigningAlgorithm::Rsa),
        Some("ed25519") => Ok(DkimSigningAlgorithm::Ed25519),
        Some(other) => anyhow::bail!("unknown value for env var {key}: {other}"),
    }
}

fn build_config(
    domain: &str,
    selector: String,
    key: &str,
    algorithm: DkimSigningAlgorithm,
) -> anyhow::Result<DkimConfig> {
    if matches!(algorithm, DkimSigningAlgorithm::Rsa) && key.contains("BEGIN PRIVATE KEY") {
        anyhow::bail!(
            "PKCS#8 keys are not supported; convert with `openssl rsa -traditional -in key.pem -out key-pkcs1.pem`"
        );
    }
    // The lettre error can describe the key material, so only its kind is reported.
    let key = DkimSigningKey::new(key, algorithm)
        .map_err(|_| anyhow::anyhow!("the private key is not a valid {algorithm} key"))?;
    let headers = SIGNED_HEADERS
        .iter()
        .map(|h| HeaderName::new_from_ascii_str(h))
        .collect();
    Ok(DkimConfig::new(
        selector,
        domain.to_owned(),
        key,
        headers,
        DkimCanonicalization {
            header: DkimCanonicalizationType::Relaxed,
            body: DkimCanonicalizationType::Relaxed,
        },
    ))
}

#[cfg(test)]
pub(crate) mod tests {
    use std::collections::HashMap;
    use std::env::VarError;
    use std::path::Path;

    use super::DkimSigners;

    /// Test-only Ed25519 seed (base64 of 32 bytes); never use outside tests.
    pub(crate) const TEST_ED25519_SEED: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=";

    fn lookup(vars: HashMap<String, String>) -> impl Fn(&str) -> Result<String, VarError> {
        move |k| vars.get(k).cloned().ok_or(VarError::NotPresent)
    }

    fn vars(entries: &[(&str, &Path, &str)]) -> HashMap<String, String> {
        let mut v = HashMap::new();
        let names: Vec<_> = entries.iter().map(|e| e.0.to_uppercase()).collect();
        v.insert("CATAPULTE_DKIM_DOMAINS".to_owned(), names.join(","));
        for (domain, file, algo) in entries {
            let p = format!("CATAPULTE_DKIM_{}", domain.to_uppercase());
            v.insert(format!("{p}_DOMAIN"), (*domain).to_owned());
            v.insert(format!("{p}_SELECTOR"), "s1".to_owned());
            v.insert(
                format!("{p}_PRIVATE_KEY_FILE"),
                file.to_string_lossy().into_owned(),
            );
            v.insert(format!("{p}_ALGORITHM"), (*algo).to_owned());
        }
        v
    }

    fn key_file(dir: &tempfile::TempDir, content: &str) -> std::path::PathBuf {
        let file = tempfile::NamedTempFile::new_in(dir.path()).unwrap();
        let (_, path) = file.keep().unwrap();
        std::fs::write(&path, content).unwrap();
        path
    }

    pub(crate) fn signers(domains: &[&str]) -> DkimSigners {
        let dir = tempfile::tempdir().unwrap();
        let file = key_file(&dir, &format!("{TEST_ED25519_SEED}\n"));
        let entries: Vec<_> = domains
            .iter()
            .map(|d| (*d, file.as_path(), "ed25519"))
            .collect();
        DkimSigners::from_lookup(lookup(vars(&entries))).unwrap()
    }

    #[test]
    fn no_domains_variable_means_no_signing() {
        let s = DkimSigners::from_lookup(lookup(HashMap::new())).unwrap();
        assert!(s.for_sender("example.com").is_none());
    }

    #[test]
    fn matching_is_exact_or_subdomain_longest_wins_and_case_insensitive() {
        let s = signers(&["example.com", "mail.example.com"]);
        assert!(s.for_sender("example.com").is_some());
        assert!(
            s.for_sender(&super::normalize_domain("EXAMPLE.com").unwrap())
                .is_some()
        );
        assert!(s.for_sender("news.example.com").is_some());
        assert!(s.for_sender("badexample.com").is_none());
        assert!(s.for_sender("example.com.evil.org").is_none());
        // `a.mail.example.com` must use the `mail.example.com` entry (longest match).
        assert_eq!(
            s.matching("a.mail.example.com").unwrap().0,
            "mail.example.com"
        );
        assert_eq!(s.matching("a.b.example.com").unwrap().0, "example.com");
    }

    #[test]
    fn pkcs8_rsa_key_is_rejected_with_a_conversion_hint() {
        let dir = tempfile::tempdir().unwrap();
        let file = key_file(
            &dir,
            "-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----\n",
        );
        let err =
            DkimSigners::from_lookup(lookup(vars(&[("example.com", &file, "rsa")]))).unwrap_err();
        assert!(
            format!("{err:#}").contains("openssl rsa -traditional"),
            "{err:#}"
        );
    }

    #[test]
    fn missing_key_file_unknown_algorithm_and_bad_key_fail() {
        let missing = Path::new("/nonexistent/dkim.key");
        assert!(
            DkimSigners::from_lookup(lookup(vars(&[("example.com", missing, "rsa")]))).is_err()
        );

        let dir = tempfile::tempdir().unwrap();
        let file = key_file(&dir, TEST_ED25519_SEED);
        let err =
            DkimSigners::from_lookup(lookup(vars(&[("example.com", &file, "dsa")]))).unwrap_err();
        assert!(format!("{err:#}").contains("unknown value"), "{err:#}");

        let junk = key_file(&dir, "not a key");
        assert!(
            DkimSigners::from_lookup(lookup(vars(&[("example.com", &junk, "ed25519")]))).is_err()
        );
    }

    fn load(domain: &str, selector: &str) -> anyhow::Result<DkimSigners> {
        let dir = tempfile::tempdir().unwrap();
        let file = key_file(&dir, TEST_ED25519_SEED);
        let mut v = vars(&[("main", &file, "ed25519")]);
        v.insert("CATAPULTE_DKIM_MAIN_DOMAIN".to_owned(), domain.to_owned());
        v.insert(
            "CATAPULTE_DKIM_MAIN_SELECTOR".to_owned(),
            selector.to_owned(),
        );
        DkimSigners::from_lookup(lookup(v))
    }

    #[test]
    fn idn_domain_is_loaded_as_punycode_and_invalid_domains_fail() {
        let s = load("Bücher.de", "s1").unwrap();
        assert_eq!(
            s.matching("xn--bcher-kva.de").unwrap().0,
            "xn--bcher-kva.de"
        );
        for bad in [
            "",
            "exa mple.com",
            ".example.com",
            "example.com.",
            "a..b",
            "ex_ample.com",
        ] {
            assert!(load(bad, "s1").is_err(), "{bad:?}");
        }
    }

    #[test]
    fn selector_follows_rfc_6376_subdomain_syntax() {
        for ok in ["s2026", "mail.s1"] {
            assert!(load("example.com", ok).is_ok(), "{ok}");
        }
        let too_long = "a".repeat(64);
        for bad in [
            "",
            "bad; d=evil.example",
            "a b",
            "-x",
            "x-",
            "a..b",
            too_long.as_str(),
        ] {
            let err = load("example.com", bad).expect_err(bad);
            assert!(format!("{err:#}").contains("_SELECTOR"), "{bad}: {err:#}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn non_unicode_domains_variable_fails_but_blank_ones_do_not() {
        use std::os::unix::ffi::OsStringExt;

        let not_unicode = |k: &str| {
            (k == "CATAPULTE_DKIM_DOMAINS")
                .then(|| VarError::NotUnicode(std::ffi::OsString::from_vec(vec![0xff])))
                .map_or(Err(VarError::NotPresent), Err)
        };
        assert!(DkimSigners::from_lookup(not_unicode).is_err());

        for blank in ["", "  ", " , ,"] {
            let v = HashMap::from([("CATAPULTE_DKIM_DOMAINS".to_owned(), blank.to_owned())]);
            assert!(
                DkimSigners::from_lookup(lookup(v))
                    .unwrap()
                    .entries
                    .is_empty()
            );
        }
    }
}
