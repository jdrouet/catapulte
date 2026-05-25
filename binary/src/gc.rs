use std::collections::HashSet;
use std::time::Duration;

use catapulte_domain::entity::attachment::BlobRef;
use catapulte_domain::port::email_repository::EmailRepository;
use catapulte_outbound_attachment_fs::store::FsAttachmentStore;
use tokio_util::sync::CancellationToken;

use crate::storage::StorageAdapter;

pub struct AttachmentGc {
    repository: StorageAdapter,
    store: FsAttachmentStore,
    sweep_interval: Duration,
    grace_period: Duration,
}

impl AttachmentGc {
    #[must_use]
    pub fn new(
        repository: StorageAdapter,
        store: FsAttachmentStore,
        sweep_interval: Duration,
        grace_period: Duration,
    ) -> Self {
        Self {
            repository,
            store,
            sweep_interval,
            grace_period,
        }
    }

    pub async fn run(self, cancel: CancellationToken) {
        loop {
            tokio::select! {
                biased;
                () = cancel.cancelled() => break,
                () = tokio::time::sleep(self.sweep_interval) => {
                    if let Err(e) = self.sweep_once(&cancel).await {
                        tracing::warn!(error = %e, "attachment gc sweep failed");
                    }
                }
            }
        }
        tracing::info!("attachment gc stopped");
    }

    /// # Errors
    ///
    /// Returns an error when listing blobs from the repository or from the fs store fails.
    pub async fn sweep_once(&self, cancel: &CancellationToken) -> anyhow::Result<()> {
        if cancel.is_cancelled() {
            return Ok(());
        }

        let live_blobs = self.repository.list_all_attachment_blobs().await?;
        let live: HashSet<String> = live_blobs
            .into_iter()
            .filter(|b| b.backend == "fs")
            .map(|b| b.key)
            .collect();

        let on_disk = self.store.list_keys_older_than(self.grace_period).await?;
        for key in on_disk {
            if cancel.is_cancelled() {
                return Ok(());
            }
            if !live.contains(&key) {
                let blob = BlobRef {
                    backend: "fs".into(),
                    key,
                };
                if let Err(e) = catapulte_domain::port::attachment_store::AttachmentStore::delete(
                    &self.store,
                    &blob,
                )
                .await
                {
                    tracing::warn!(
                        error = %e,
                        blob_key = %blob.key,
                        "gc failed to delete orphaned blob"
                    );
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use catapulte_domain::entity::attachment::AttachmentRef;
    use catapulte_domain::entity::body::{BodySource, Plain};
    use catapulte_domain::entity::email::{EmailId, RecipientKind};
    use catapulte_domain::entity::envelope::Envelope;
    use catapulte_domain::port::attachment_store::AttachmentStore;
    use catapulte_domain::port::email_repository::EmailRepository;
    use catapulte_outbound_attachment_fs::store::FsAttachmentStore;

    use super::AttachmentGc;
    use crate::storage::StorageAdapter;

    async fn fresh_sqlite() -> (StorageAdapter, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("test.db");
        let config = catapulte_outbound_sqlite::SqliteConfig {
            url: format!("sqlite:{}", db_path.display()),
        };
        let adapter = config.build().await.expect("sqlite build");
        adapter.migrate().await.expect("sqlite migrate");
        (StorageAdapter::Sqlite(adapter), dir)
    }

    fn sample_envelope(attachments: Vec<AttachmentRef>) -> Envelope {
        Envelope {
            idempotency_key: None,
            subject: None,
            sender: "sender@example.com".to_owned(),
            recipients: vec![(RecipientKind::To, "to@example.com".to_owned())],
            body: BodySource::Plain(Plain::try_new(Some("hi".to_owned()), None).unwrap()),
            variables: serde_json::Map::new(),
            attachments,
        }
    }

    #[tokio::test]
    async fn sweep_once_removes_orphaned_blobs_only() {
        let (storage, _storage_dir) = fresh_sqlite().await;

        let store_dir = tempfile::tempdir().expect("store dir");
        let fs_store = FsAttachmentStore::new(store_dir.path().to_path_buf())
            .await
            .expect("fs store");

        // Write three blobs to disk.
        let blob1 = fs_store
            .put(Box::pin(Cursor::new(b"content1".to_vec())))
            .await
            .expect("put blob1");
        let blob2 = fs_store
            .put(Box::pin(Cursor::new(b"content2".to_vec())))
            .await
            .expect("put blob2");
        let orphan = fs_store
            .put(Box::pin(Cursor::new(b"orphan".to_vec())))
            .await
            .expect("put orphan");

        // Save one email referencing blob1 and blob2.
        let id = EmailId::default();
        storage
            .save(id, &sample_envelope(vec![]))
            .await
            .expect("save");
        storage
            .set_attachments(
                id,
                &[
                    AttachmentRef {
                        filename: "a.txt".into(),
                        content_type: "text/plain".into(),
                        size_bytes: 8,
                        blob: blob1.blob.clone(),
                    },
                    AttachmentRef {
                        filename: "b.txt".into(),
                        content_type: "text/plain".into(),
                        size_bytes: 8,
                        blob: blob2.blob.clone(),
                    },
                ],
            )
            .await
            .expect("set_attachments");

        let cancel = tokio_util::sync::CancellationToken::new();
        let gc = AttachmentGc::new(
            storage,
            fs_store.clone(),
            std::time::Duration::from_secs(3600),
            std::time::Duration::ZERO,
        );
        gc.sweep_once(&cancel).await.expect("sweep_once");

        // blob1 and blob2 must still exist; orphan must be gone.
        let mut remaining = fs_store.list_keys().await.expect("list_keys");
        remaining.sort();
        let mut expected = vec![blob1.blob.key.clone(), blob2.blob.key.clone()];
        expected.sort();
        assert_eq!(
            remaining, expected,
            "orphan should have been removed, live blobs should remain"
        );

        assert!(
            fs_store.get(&orphan.blob).await.is_err(),
            "orphaned blob should be gone from disk"
        );
    }
}
