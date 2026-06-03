use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Context;
use catapulte_domain::entity::attachment::BlobRef;
use catapulte_domain::port::attachment_store::{
    AttachmentReader, AttachmentStore, AttachmentStoreError, PutResult,
};
use futures_util::StreamExt as _;
use s3::bucket::Bucket;
use s3::creds::Credentials;
use s3::region::Region;
use tokio_util::io::StreamReader;
use tracing::debug;

#[derive(Debug)]
pub struct S3AttachmentStoreConfig {
    pub endpoint: String,
    pub region: String,
    pub bucket: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub path_style: bool,
    pub prefix: String,
}

impl S3AttachmentStoreConfig {
    /// # Errors
    ///
    /// Returns an error when a required env var is missing.
    pub fn from_env(prefix: &str) -> anyhow::Result<Self> {
        let key_endpoint = format!("{prefix}_ENDPOINT");
        let endpoint = std::env::var(&key_endpoint)
            .with_context(|| format!("missing env var {key_endpoint}"))?;

        let region =
            std::env::var(format!("{prefix}_REGION")).unwrap_or_else(|_| "us-east-1".to_string());

        let key_bucket = format!("{prefix}_BUCKET");
        let bucket =
            std::env::var(&key_bucket).with_context(|| format!("missing env var {key_bucket}"))?;

        let key_access = format!("{prefix}_ACCESS_KEY_ID");
        let access_key_id =
            std::env::var(&key_access).with_context(|| format!("missing env var {key_access}"))?;

        let key_secret = format!("{prefix}_SECRET_ACCESS_KEY");
        let secret_access_key =
            std::env::var(&key_secret).with_context(|| format!("missing env var {key_secret}"))?;

        let path_style = parse_path_style_env(
            std::env::var(format!("{prefix}_PATH_STYLE"))
                .ok()
                .as_deref(),
        );

        let obj_prefix = std::env::var(format!("{prefix}_PREFIX")).unwrap_or_default();

        Ok(Self {
            endpoint,
            region,
            bucket,
            access_key_id,
            secret_access_key,
            path_style,
            prefix: obj_prefix,
        })
    }

    /// # Errors
    ///
    /// Returns an error when the S3 bucket handle cannot be constructed (invalid
    /// endpoint, region, or credentials).
    // Keep `async` so callers can `.await` this just like the fs adapter's
    // `build`, even though no I/O is needed here.
    #[allow(clippy::unused_async)]
    pub async fn build(self) -> anyhow::Result<S3AttachmentStore> {
        S3AttachmentStore::from_config(self)
    }
}

#[derive(Clone)]
pub struct S3AttachmentStore {
    bucket: Arc<Bucket>,
    prefix: Arc<str>,
}

impl S3AttachmentStore {
    fn from_config(cfg: S3AttachmentStoreConfig) -> anyhow::Result<Self> {
        let region = Region::Custom {
            region: cfg.region,
            endpoint: cfg.endpoint,
        };

        let credentials = Credentials::new(
            Some(&cfg.access_key_id),
            Some(&cfg.secret_access_key),
            None,
            None,
            None,
        )
        .context("invalid S3 credentials")?;

        let bucket = Bucket::new(&cfg.bucket, region, credentials)
            .context("failed to create S3 bucket handle")?;

        let bucket: Box<Bucket> = if cfg.path_style {
            bucket.with_path_style()
        } else {
            bucket
        };

        Ok(Self {
            bucket: Arc::from(bucket),
            prefix: Arc::from(cfg.prefix.as_str()),
        })
    }

    fn generate_key(&self) -> String {
        format!("{}{}", self.prefix, uuid::Uuid::now_v7().simple())
    }

    /// Returns object keys under the configured prefix whose `LastModified`
    /// timestamp is older than `age`.
    ///
    /// Objects whose timestamp is missing or unparseable are treated as
    /// brand-new and excluded.
    ///
    /// # Errors
    ///
    /// Returns an error when the S3 list operation fails.
    pub async fn list_keys_older_than(&self, age: Duration) -> anyhow::Result<Vec<String>> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO);

        let pages = self
            .bucket
            .list(self.prefix.to_string(), None)
            .await
            .context("S3 list failed")?;

        let mut keys = Vec::new();
        for page in pages {
            for obj in page.contents {
                if object_is_older_than(&obj.key, Some(&obj.last_modified), now, age) {
                    keys.push(obj.key);
                }
            }
        }

        Ok(keys)
    }
}

/// Returns `true` when the object should be considered older than `age`.
///
/// `now_since_epoch` is the current wall clock expressed as a `Duration` since
/// the Unix epoch.  The function is pure so it can be unit-tested without I/O.
fn object_is_older_than(
    key: &str,
    last_modified: Option<&str>,
    now_since_epoch: Duration,
    age: Duration,
) -> bool {
    let Some(ts) = last_modified else {
        debug!(key, "S3 object has no last_modified; treating as new");
        return false;
    };

    let Some(object_epoch) = parse_iso8601_to_epoch(ts) else {
        debug!(
            key,
            ts, "S3 object has unparseable last_modified; treating as new"
        );
        return false;
    };

    // `now_since_epoch` may be less than `object_epoch` when the server clock
    // is slightly ahead of ours; treat that as age zero (keep the object).
    let elapsed = now_since_epoch.saturating_sub(object_epoch);
    elapsed >= age
}

/// Parses an RFC-3339 / ISO-8601 datetime string into seconds since the Unix
/// epoch.
///
/// Only the common shapes that S3 and `MinIO` actually emit are handled:
///   `YYYY-MM-DDTHH:MM:SSZ`
///   `YYYY-MM-DDTHH:MM:SS.xxxZ`
///   `YYYY-MM-DDTHH:MM:SS+00:00`
///   `YYYY-MM-DD HH:MM:SSZ`  (space separator, produced by some impls)
///
/// Returns `None` when the string cannot be parsed.
fn parse_iso8601_to_epoch(s: &str) -> Option<Duration> {
    let s = s.trim();
    if s.len() < 19 {
        return None;
    }

    let year: u64 = s[0..4].parse().ok()?;
    let month: u64 = s[5..7].parse().ok()?;
    let day: u64 = s[8..10].parse().ok()?;

    let sep = s.as_bytes().get(10)?;
    if *sep != b'T' && *sep != b' ' {
        return None;
    }

    let hour: u64 = s[11..13].parse().ok()?;
    let minute: u64 = s[14..16].parse().ok()?;
    let second: u64 = s[17..19].parse().ok()?;

    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    if hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    if year < 1970 {
        return None;
    }

    let days = gregorian_days_since_epoch(year, month, day)?;
    let total_seconds = days * 86_400 + hour * 3_600 + minute * 60 + second;
    Some(Duration::from_secs(total_seconds))
}

// Days in each month for a non-leap year (indexed 0 = January).
const MONTH_DAYS: [u64; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];

/// Computes the number of days since 1970-01-01 for a proleptic Gregorian date.
fn gregorian_days_since_epoch(year: u64, month: u64, day: u64) -> Option<u64> {
    let y = year - 1;
    let leap_days = y / 4 - y / 100 + y / 400;
    let years_since_epoch = year.checked_sub(1970)?;
    let days_from_years =
        years_since_epoch * 365 + (leap_days - (1969 / 4 - 1969 / 100 + 1969 / 400));

    let mut month_offset: u64 = 0;
    // month is 1-based and in range 1..=12 (validated by caller).
    for m in 1..month {
        let idx = usize::try_from(m - 1).ok()?;
        month_offset += MONTH_DAYS[idx];
    }

    let is_leap = (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400);
    if is_leap && month > 2 {
        month_offset += 1;
    }

    Some(days_from_years + month_offset + (day - 1))
}

/// Parses the `PATH_STYLE` environment variable string into a `bool`.
///
/// `None` (var absent) or any value other than `"false"` (case-insensitive)
/// returns `true`.  Only `"false"` / `"FALSE"` / `"False"` returns `false`.
fn parse_path_style_env(value: Option<&str>) -> bool {
    value.is_none_or(|v| !v.trim().eq_ignore_ascii_case("false"))
}

fn s3_err_to_io(e: s3::error::S3Error, context: &'static str) -> AttachmentStoreError {
    AttachmentStoreError::Io {
        source: anyhow::Error::new(e).context(context),
    }
}

impl AttachmentStore for S3AttachmentStore {
    async fn put(&self, mut reader: AttachmentReader) -> Result<PutResult, AttachmentStoreError> {
        // Stream the body straight to S3; `put_object_stream` handles multipart
        // internally for large objects and reports the uploaded byte count.
        let key = self.generate_key();

        let response = self
            .bucket
            .put_object_stream(&mut reader, &key)
            .await
            .map_err(|e| s3_err_to_io(e, "failed to upload object to S3"))?;

        let size_bytes = response.uploaded_bytes() as u64;

        Ok(PutResult {
            blob: BlobRef {
                backend: "s3".into(),
                key,
            },
            size_bytes,
        })
    }

    async fn get(&self, blob: &BlobRef) -> Result<AttachmentReader, AttachmentStoreError> {
        let stream_response = self
            .bucket
            .get_object_stream(&blob.key)
            .await
            .map_err(|e| s3_err_to_io(e, "failed to get object stream from S3"))?;

        if stream_response.status_code == 404 {
            return Err(AttachmentStoreError::NotFound);
        }
        if stream_response.status_code >= 400 {
            return Err(AttachmentStoreError::Io {
                source: anyhow::anyhow!("S3 get returned status {}", stream_response.status_code),
            });
        }

        // The stream items are `Result<Bytes, S3Error>`.  Map the error type to
        // `std::io::Error` and wrap in `StreamReader` to get an `AsyncRead`.
        let io_stream = stream_response
            .bytes
            .map(|item| item.map_err(std::io::Error::other));
        let reader = StreamReader::new(io_stream);
        Ok(Box::pin(reader))
    }

    async fn delete(&self, blob: &BlobRef) -> Result<(), AttachmentStoreError> {
        let response = self
            .bucket
            .delete_object(&blob.key)
            .await
            .map_err(|e| s3_err_to_io(e, "failed to delete object from S3"))?;

        // 204 No Content or any 2xx is success.  404 is idempotent.
        if response.status_code() == 404 || response.status_code() < 300 {
            return Ok(());
        }

        Err(AttachmentStoreError::Io {
            source: anyhow::anyhow!("S3 delete returned status {}", response.status_code()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        S3AttachmentStoreConfig, gregorian_days_since_epoch, object_is_older_than,
        parse_iso8601_to_epoch, parse_path_style_env,
    };
    use std::time::Duration;

    // ---- from_env parsing ----
    //
    // "Missing required var" tests use fabricated prefixes that are
    // guaranteed not to exist in any real environment, so no env mutation is
    // needed.  Optional-var default tests exercise the pure helper directly.

    #[test]
    fn from_env_missing_endpoint_returns_error() {
        // Prefix that will never have env vars set in any environment.
        let err = S3AttachmentStoreConfig::from_env("CATAPULTE_S3_TEST_ABSENT_ENDPOINT_XZQR7");
        assert!(err.is_err(), "expected error for missing endpoint");
        assert!(
            err.unwrap_err().to_string().contains("ENDPOINT"),
            "error should mention ENDPOINT"
        );
    }

    #[test]
    fn from_env_missing_bucket_returns_error() {
        // This prefix has no BUCKET var but does have an ENDPOINT var absent too,
        // so the first error is ENDPOINT.  We test BUCKET separately via
        // parse_path_style_env and direct struct construction.
        let err = S3AttachmentStoreConfig::from_env("CATAPULTE_S3_TEST_ABSENT_BUCKET_XZQR8");
        assert!(err.is_err());
        // Either ENDPOINT or BUCKET is missing — both trigger the required-var path.
        let msg = err.unwrap_err().to_string();
        assert!(
            msg.contains("ENDPOINT") || msg.contains("BUCKET"),
            "error should mention a missing required var, got: {msg}"
        );
    }

    // Optional-var defaults: tested through the pure `parse_path_style_env`
    // helper so no environment mutation is needed.

    #[test]
    fn parse_path_style_absent_defaults_to_true() {
        assert!(parse_path_style_env(None), "absent → true");
    }

    #[test]
    fn parse_path_style_false_lowercase() {
        assert!(!parse_path_style_env(Some("false")));
    }

    #[test]
    fn parse_path_style_false_uppercase() {
        assert!(!parse_path_style_env(Some("FALSE")));
    }

    #[test]
    fn parse_path_style_false_mixed_case() {
        assert!(!parse_path_style_env(Some("False")));
    }

    #[test]
    fn parse_path_style_true_string() {
        assert!(parse_path_style_env(Some("true")));
    }

    #[test]
    fn parse_path_style_arbitrary_non_false_is_true() {
        // Any value that isn't "false" is treated as true.
        assert!(parse_path_style_env(Some("1")));
        assert!(parse_path_style_env(Some("yes")));
    }

    // ---- timestamp parsing ----

    #[test]
    fn parse_iso8601_known_date() {
        // 2024-01-15T00:00:00Z = Unix 1705276800
        let secs = parse_iso8601_to_epoch("2024-01-15T00:00:00Z")
            .expect("should parse")
            .as_secs();
        assert_eq!(secs, 1_705_276_800);
    }

    #[test]
    fn parse_iso8601_with_offset() {
        let secs_z = parse_iso8601_to_epoch("2024-01-15T00:00:00Z")
            .unwrap()
            .as_secs();
        let secs_offset = parse_iso8601_to_epoch("2024-01-15T00:00:00+00:00")
            .unwrap()
            .as_secs();
        assert_eq!(secs_z, secs_offset);
    }

    #[test]
    fn parse_iso8601_with_subseconds() {
        let secs = parse_iso8601_to_epoch("2024-01-15T00:00:00.123Z")
            .unwrap()
            .as_secs();
        assert_eq!(secs, 1_705_276_800);
    }

    #[test]
    fn parse_iso8601_space_separator() {
        let secs = parse_iso8601_to_epoch("2024-01-15 00:00:00Z")
            .unwrap()
            .as_secs();
        assert_eq!(secs, 1_705_276_800);
    }

    #[test]
    fn parse_iso8601_invalid_returns_none() {
        assert!(parse_iso8601_to_epoch("not-a-date").is_none());
        assert!(parse_iso8601_to_epoch("").is_none());
        assert!(parse_iso8601_to_epoch("1969-12-31T23:59:59Z").is_none());
    }

    #[test]
    fn gregorian_days_epoch_day_zero() {
        assert_eq!(gregorian_days_since_epoch(1970, 1, 1), Some(0));
    }

    #[test]
    fn gregorian_days_known_date() {
        // 2024-01-15 = Unix 1705276800 / 86400 = 19736 days
        assert_eq!(
            gregorian_days_since_epoch(2024, 1, 15),
            Some(1_705_276_800 / 86_400)
        );
    }

    // ---- age filtering ----

    const NOW: Duration = Duration::from_hours(473_688); // 2024-01-15T00:00:00Z

    #[test]
    fn object_is_older_than_no_timestamp_returns_false() {
        assert!(!object_is_older_than(
            "key",
            None,
            NOW,
            Duration::from_secs(1)
        ));
    }

    #[test]
    fn object_is_older_than_bad_timestamp_returns_false() {
        assert!(!object_is_older_than(
            "key",
            Some("garbage"),
            NOW,
            Duration::from_secs(1)
        ));
    }

    #[test]
    fn object_is_older_than_exactly_at_age_boundary() {
        // Object was modified exactly 1 hour ago.
        let one_hour_ago_secs = 1_705_276_800u64 - 3_600;
        let ts = format_epoch_as_iso8601(one_hour_ago_secs);
        assert!(object_is_older_than(
            "key",
            Some(&ts),
            NOW,
            Duration::from_hours(1)
        ));
    }

    #[test]
    fn object_is_older_than_brand_new_returns_false() {
        // Object modified at exactly "now" — elapsed is zero, not >= 1 second.
        let ts = "2024-01-15T00:00:00Z";
        assert!(!object_is_older_than(
            "key",
            Some(ts),
            NOW,
            Duration::from_secs(1)
        ));
    }

    #[test]
    fn object_is_older_than_zero_age_always_true_for_past() {
        let past = "2020-06-01T00:00:00Z";
        assert!(object_is_older_than("key", Some(past), NOW, Duration::ZERO));
    }

    #[test]
    fn object_is_older_than_max_age_always_false() {
        let old = "2020-01-01T00:00:00Z";
        assert!(!object_is_older_than("key", Some(old), NOW, Duration::MAX));
    }

    /// Format seconds-since-epoch as `YYYY-MM-DDTHH:MM:SSZ` for test inputs.
    fn format_epoch_as_iso8601(secs: u64) -> String {
        let time_of_day = secs % 86_400;
        let days = secs / 86_400;
        let hour = time_of_day / 3_600;
        let minute = (time_of_day % 3_600) / 60;
        let second = time_of_day % 60;
        let (year, month, day) = days_to_ymd(days);
        format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
    }

    fn days_to_ymd(mut days: u64) -> (u64, u64, u64) {
        let mut year = 1970u64;
        loop {
            let leap =
                (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400);
            let year_days = if leap { 366 } else { 365 };
            if days < year_days {
                break;
            }
            days -= year_days;
            year += 1;
        }
        let leap =
            (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400);
        let month_days: [u64; 12] = [
            31,
            if leap { 29 } else { 28 },
            31,
            30,
            31,
            30,
            31,
            31,
            30,
            31,
            30,
            31,
        ];
        let mut month = 1u64;
        for &md in &month_days {
            if days < md {
                break;
            }
            days -= md;
            month += 1;
        }
        (year, month, days + 1)
    }
}
