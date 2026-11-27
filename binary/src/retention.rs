use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio_util::sync::CancellationToken;

use crate::storage::StorageAdapter;

const PURGE_INTERVAL: Duration = Duration::from_secs(3600);
const PURGE_BATCH: u32 = 500;

pub struct RetentionPurge {
    storage: StorageAdapter,
    retention: Duration,
}

impl RetentionPurge {
    #[must_use]
    pub fn new(storage: StorageAdapter, retention: Duration) -> Self {
        Self { storage, retention }
    }

    pub async fn run(self, cancel: CancellationToken) {
        loop {
            tokio::select! {
                biased;
                () = cancel.cancelled() => break,
                () = tokio::time::sleep(PURGE_INTERVAL) => {
                    self.purge_once(&cancel).await;
                }
            }
        }
    }

    /// Deletes finished emails older than the retention, in batches, and returns how many.
    pub async fn purge_once(&self, cancel: &CancellationToken) -> u64 {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
        let retention_ms = i64::try_from(self.retention.as_millis()).unwrap_or(i64::MAX);
        let cutoff_ms = now_ms.saturating_sub(retention_ms);
        let storage = &self.storage;
        sweep(cancel, PURGE_BATCH, |limit| async move {
            storage.purge_expired(cutoff_ms, limit).await
        })
        .await
    }
}

/// Runs batches until one comes back short, fails, or the token is cancelled; logs once.
async fn sweep<F>(cancel: &CancellationToken, batch: u32, mut next: impl FnMut(u32) -> F) -> u64
where
    F: Future<Output = anyhow::Result<u64>>,
{
    let mut total = 0;
    while !cancel.is_cancelled() {
        match next(batch).await {
            Ok(n) => {
                total += n;
                if n < u64::from(batch) {
                    break;
                }
                tokio::task::yield_now().await;
            }
            Err(e) => {
                tracing::warn!(error = %e, "retention purge failed");
                break;
            }
        }
    }
    tracing::info!(purged = total, "retention purge finished");
    total
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use catapulte_domain::entity::body::{BodySource, Plain};
    use catapulte_domain::entity::email::{EmailId, RecipientKind};
    use catapulte_domain::entity::envelope::Envelope;
    use catapulte_domain::entity::lifecycle_event::{EventContext, LifecycleEvent};
    use catapulte_domain::entity::sender::SenderName;
    use catapulte_domain::port::email_repository::{EmailRepository, ListEmailsParams};
    use catapulte_domain::port::event_publisher::EventPublisher;
    use tokio_util::sync::CancellationToken;

    use super::{RetentionPurge, sweep};
    use crate::storage::StorageAdapter;

    #[derive(Clone, Default)]
    struct Logs(Arc<Mutex<Vec<u8>>>);

    impl Write for Logs {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Logs {
        fn capture() -> (Self, tracing::subscriber::DefaultGuard) {
            let logs = Self::default();
            let writer = logs.clone();
            let subscriber = tracing_subscriber::fmt()
                .with_writer(move || writer.clone())
                .with_ansi(false)
                .finish();
            (logs, tracing::subscriber::set_default(subscriber))
        }

        fn text(&self) -> String {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
        }

        fn count(&self, level: &str) -> usize {
            self.text()
                .lines()
                .filter(|l| l.contains(&format!(" {level} ")))
                .count()
        }
    }

    #[tokio::test]
    async fn sweep_logs_one_info_for_zero() {
        let (logs, _guard) = Logs::capture();

        let total = sweep(&CancellationToken::new(), 2, |_| async { Ok(0) }).await;

        assert_eq!(total, 0);
        assert_eq!(logs.count("INFO"), 1);
        assert!(logs.text().contains("purged=0"));
    }

    #[tokio::test]
    async fn sweep_runs_multiple_batches() {
        let (logs, _guard) = Logs::capture();
        let mut results = vec![1, 2, 2];
        let mut calls = 0;

        let total = sweep(&CancellationToken::new(), 2, |_| {
            calls += 1;
            std::future::ready(Ok(results.pop().unwrap()))
        })
        .await;

        assert_eq!((total, calls), (5, 3));
        assert_eq!(logs.count("INFO"), 1);
        assert!(logs.text().contains("purged=5"));
    }

    #[tokio::test]
    async fn sweep_reports_partial_progress_when_a_later_batch_fails() {
        let (logs, _guard) = Logs::capture();
        let mut calls = 0;

        let total = sweep(&CancellationToken::new(), 2, |_| {
            calls += 1;
            std::future::ready(if calls == 1 {
                Ok(2)
            } else {
                Err(anyhow::anyhow!("boom"))
            })
        })
        .await;

        assert_eq!(total, 2);
        assert_eq!(logs.count("WARN"), 1);
        assert_eq!(logs.count("INFO"), 1);
        assert!(logs.text().contains("purged=2"));
    }

    #[tokio::test]
    async fn sweep_stops_on_cancel_between_batches() {
        let (logs, _guard) = Logs::capture();
        let cancel = CancellationToken::new();
        let mut calls = 0;

        let total = sweep(&cancel, 2, |_| {
            calls += 1;
            cancel.cancel();
            std::future::ready(Ok(2))
        })
        .await;

        assert_eq!((total, calls), (2, 1));
        assert_eq!(logs.count("INFO"), 1);
    }

    async fn fresh_sqlite() -> (StorageAdapter, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = catapulte_outbound_sqlite::SqliteConfig {
            url: format!("sqlite:{}", dir.path().join("test.db").display()),
        };
        let adapter = config.build().await.expect("sqlite build");
        adapter.migrate().await.expect("sqlite migrate");
        (StorageAdapter::Sqlite(adapter), dir)
    }

    fn envelope() -> Envelope {
        Envelope {
            subject: Some("secret-subject".to_owned()),
            ..Envelope::new(
                "sender@example.com",
                vec![(RecipientKind::To, "private@example.com".to_owned())],
                BodySource::Plain(Plain::try_new(Some("hi".to_owned()), None).unwrap()),
            )
        }
    }

    async fn exists(storage: &StorageAdapter, id: EmailId) -> bool {
        let params = ListEmailsParams {
            tag: None,
            owner: None,
            status: None,
            after_ms: None,
            before_ms: None,
            recipient: None,
            template: None,
            id: Some(id),
            limit: 1,
            offset: 0,
        };
        !storage.list_emails(params).await.unwrap().is_empty()
    }

    #[tokio::test]
    async fn purge_once_deletes_finished_emails_without_logging_content() {
        let (logs, _guard) = Logs::capture();
        let (storage, _dir) = fresh_sqlite().await;
        let finished = EmailId::default();
        let unfinished = EmailId::default();
        storage.save(finished, &envelope()).await.unwrap();
        storage.save(unfinished, &envelope()).await.unwrap();
        storage
            .publish(&LifecycleEvent::Sent {
                id: finished,
                sender_name: SenderName::new("test"),
                context: EventContext::with_correlation(None),
                smtp_response: None,
                message_id: "<m@example.com>".to_owned(),
            })
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;

        let purge = RetentionPurge::new(storage.clone(), Duration::ZERO);
        let purged = purge.purge_once(&CancellationToken::new()).await;

        assert_eq!(purged, 1);
        assert!(!exists(&storage, finished).await);
        assert!(exists(&storage, unfinished).await);
        assert_eq!(logs.count("INFO"), 1);
        let text = logs.text();
        assert!(!text.contains("private@example.com") && !text.contains("secret-subject"));
    }
}
