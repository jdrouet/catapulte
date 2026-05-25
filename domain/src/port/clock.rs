/// Provides the current wall-clock time as a Unix-epoch millisecond timestamp.
pub trait Clock: Send + Sync + 'static {
    /// Returns the current time as milliseconds since the Unix epoch.
    fn now_ms(&self) -> i64;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time before Unix epoch")
                .as_millis(),
        )
        .unwrap_or(i64::MAX)
    }
}
