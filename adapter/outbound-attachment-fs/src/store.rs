use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use catapulte_domain::entity::attachment::BlobRef;
use catapulte_domain::port::attachment_store::{
    AttachmentReader, AttachmentStore, AttachmentStoreError, PutResult,
};
use tokio::io::AsyncWriteExt;

pub struct FsAttachmentStoreConfig {
    pub root: PathBuf,
}

impl FsAttachmentStoreConfig {
    /// # Errors
    /// Returns an error when `<prefix>_ROOT` env var is missing.
    pub fn from_env(prefix: &str) -> anyhow::Result<Self> {
        let key = format!("{prefix}_ROOT");
        let root = std::env::var(&key).with_context(|| format!("missing env var {key}"))?;
        Ok(Self {
            root: PathBuf::from(root),
        })
    }

    /// # Errors
    /// Returns an error when the root directory cannot be prepared.
    pub async fn build(self) -> anyhow::Result<FsAttachmentStore> {
        FsAttachmentStore::new(self.root).await
    }
}

#[derive(Clone)]
pub struct FsAttachmentStore {
    root: Arc<PathBuf>,
}

impl FsAttachmentStore {
    /// Creates the root and `<root>/.tmp` directories if missing.
    ///
    /// # Errors
    /// Returns an error when the root or tmp directory cannot be created.
    pub async fn new(root: PathBuf) -> anyhow::Result<Self> {
        let tmp = root.join(".tmp");
        tokio::fs::create_dir_all(&tmp)
            .await
            .with_context(|| format!("failed to create directory {}", tmp.display()))?;
        Ok(Self {
            root: Arc::new(root),
        })
    }

    fn tmp_dir(&self) -> PathBuf {
        self.root.join(".tmp")
    }

    /// Returns every committed blob key under root (excludes tempfiles
    /// and any non-blob files). Caller uses this for GC by diffing
    /// against the live set known to the application.
    ///
    /// # Errors
    /// Returns an error if the root directory cannot be read.
    pub async fn list_keys(&self) -> anyhow::Result<Vec<String>> {
        let mut entries = tokio::fs::read_dir(self.root.as_ref())
            .await
            .with_context(|| format!("failed to read directory {}", self.root.display()))?;

        let mut keys = Vec::new();
        while let Some(entry) = entries.next_entry().await.context("failed to read entry")? {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            // Skip hidden files (including the .tmp directory) and anything that
            // isn't a plain file so that directory entries don't leak into GC.
            if name.starts_with('.') {
                continue;
            }
            let ft = entry.file_type().await.context("failed to stat entry")?;
            if ft.is_file() {
                keys.push(name.into_owned());
            }
        }
        Ok(keys)
    }

    /// Removes tempfiles older than `older_than`. Safe to run while puts
    /// are in flight (concurrent puts use distinct uuid filenames).
    ///
    /// # Errors
    /// Returns an error if the tmp directory cannot be enumerated.
    pub async fn cleanup_temp(&self, older_than: Duration) -> anyhow::Result<usize> {
        let tmp = self.tmp_dir();
        let mut entries = tokio::fs::read_dir(&tmp)
            .await
            .with_context(|| format!("failed to read tmp directory {}", tmp.display()))?;

        let mut removed = 0usize;
        while let Some(entry) = entries
            .next_entry()
            .await
            .context("failed to read tmp entry")?
        {
            let Ok(meta) = entry.metadata().await else {
                continue;
            };
            let Ok(modified) = meta.modified() else {
                continue;
            };
            let age = modified.elapsed().unwrap_or(Duration::MAX);
            if age >= older_than && tokio::fs::remove_file(entry.path()).await.is_ok() {
                removed += 1;
            }
        }
        Ok(removed)
    }
}

fn is_dangerous_key(key: &str) -> bool {
    key.starts_with('.') || key.contains('/') || key.contains('\\')
}

impl FsAttachmentStore {
    /// Writes `reader` to `tmp_path`, then atomically renames it to
    /// `final_path`. Returns the number of bytes written.
    ///
    /// On success the parent directory is fsynced (unix only) so the new
    /// directory entry survives a crash.
    async fn write_committed(
        &self,
        tmp_path: &std::path::Path,
        reader: &mut AttachmentReader,
        final_path: &std::path::Path,
    ) -> Result<u64, AttachmentStoreError> {
        let mut file =
            tokio::fs::File::create(tmp_path)
                .await
                .map_err(|e| AttachmentStoreError::Io {
                    source: anyhow::Error::new(e).context("failed to create tmp file"),
                })?;

        let size_bytes =
            tokio::io::copy(reader, &mut file)
                .await
                .map_err(|e| AttachmentStoreError::Io {
                    source: anyhow::Error::new(e).context("failed to write to tmp file"),
                })?;

        file.flush().await.map_err(|e| AttachmentStoreError::Io {
            source: anyhow::Error::new(e).context("failed to flush tmp file"),
        })?;
        file.sync_all()
            .await
            .map_err(|e| AttachmentStoreError::Io {
                source: anyhow::Error::new(e).context("failed to fsync tmp file"),
            })?;

        // Drop the file handle before rename to avoid issues on some platforms.
        drop(file);

        tokio::fs::rename(tmp_path, final_path)
            .await
            .map_err(|e| AttachmentStoreError::Io {
                source: anyhow::Error::new(e).context("failed to rename tmp file to final path"),
            })?;

        // Fsync the parent directory so the new directory entry survives a
        // crash. Without this, rename is only namespace-visible to running
        // processes — after a power loss the dir entry can disappear.
        #[cfg(unix)]
        tokio::fs::File::open(self.root.as_ref())
            .await
            .map_err(|e| AttachmentStoreError::Io {
                source: anyhow::Error::new(e).context("failed to open root dir for fsync"),
            })?
            .sync_all()
            .await
            .map_err(|e| AttachmentStoreError::Io {
                source: anyhow::Error::new(e).context("failed to fsync root directory"),
            })?;

        Ok(size_bytes)
    }
}

impl AttachmentStore for FsAttachmentStore {
    async fn put(&self, mut reader: AttachmentReader) -> Result<PutResult, AttachmentStoreError> {
        let key = uuid::Uuid::now_v7().simple().to_string();
        let tmp_name = uuid::Uuid::now_v7().simple().to_string();
        let tmp_path = self.tmp_dir().join(&tmp_name);
        let final_path = self.root.join(&key);

        let result = self
            .write_committed(&tmp_path, &mut reader, &final_path)
            .await;
        if result.is_err() {
            let _ = tokio::fs::remove_file(&tmp_path).await;
        }
        let size_bytes = result?;

        Ok(PutResult {
            blob: BlobRef {
                backend: "fs".into(),
                key,
            },
            size_bytes,
        })
    }

    async fn get(&self, blob: &BlobRef) -> Result<AttachmentReader, AttachmentStoreError> {
        if is_dangerous_key(&blob.key) {
            return Err(AttachmentStoreError::NotFound);
        }

        let path = self.root.join(&blob.key);
        let file = tokio::fs::File::open(&path).await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                AttachmentStoreError::NotFound
            } else {
                AttachmentStoreError::Io {
                    source: anyhow::Error::new(e).context("failed to open blob file"),
                }
            }
        })?;

        Ok(Box::pin(file))
    }

    async fn delete(&self, blob: &BlobRef) -> Result<(), AttachmentStoreError> {
        if is_dangerous_key(&blob.key) {
            return Err(AttachmentStoreError::NotFound);
        }

        let path = self.root.join(&blob.key);
        match tokio::fs::remove_file(&path).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(AttachmentStoreError::Io {
                source: anyhow::Error::new(e).context("failed to delete blob file"),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{self, Cursor};
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use std::time::Duration;

    use catapulte_domain::entity::attachment::BlobRef;
    use catapulte_domain::port::attachment_store::{
        AttachmentReader, AttachmentStore, AttachmentStoreError,
    };
    use tokio::io::{AsyncRead, AsyncReadExt, ReadBuf};

    use super::FsAttachmentStore;

    async fn make_store() -> (FsAttachmentStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = FsAttachmentStore::new(dir.path().to_path_buf())
            .await
            .expect("new store");
        (store, dir)
    }

    fn reader_from(data: &[u8]) -> AttachmentReader {
        Box::pin(Cursor::new(data.to_vec()))
    }

    // A reader that yields `prefix` bytes then returns an I/O error.
    struct ErrorAfter {
        data: Vec<u8>,
        pos: usize,
    }

    impl ErrorAfter {
        fn new(data: Vec<u8>) -> Self {
            Self { data, pos: 0 }
        }
    }

    impl AsyncRead for ErrorAfter {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            if self.pos < self.data.len() {
                let n = buf.remaining().min(self.data.len() - self.pos);
                buf.put_slice(&self.data[self.pos..self.pos + n]);
                self.pos += n;
                Poll::Ready(Ok(()))
            } else {
                Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "simulated read error",
                )))
            }
        }
    }

    #[tokio::test]
    async fn put_then_get_roundtrips_bytes() {
        let (store, _dir) = make_store().await;
        let payload = b"hello, world!";
        let result = store.put(reader_from(payload)).await.expect("put");

        let mut reader = store.get(&result.blob).await.expect("get");
        let mut buf = Vec::new();
        reader.read_to_end(&mut buf).await.expect("read");
        assert_eq!(buf, payload);
    }

    #[tokio::test]
    async fn put_returns_size_matching_input() {
        let (store, _dir) = make_store().await;
        let payload = b"some data here";
        let result = store.put(reader_from(payload)).await.expect("put");
        assert_eq!(result.size_bytes, payload.len() as u64);
    }

    #[tokio::test]
    async fn delete_idempotent() {
        let (store, _dir) = make_store().await;
        let blob = BlobRef {
            backend: "fs".into(),
            key: "nonexistent-key".into(),
        };
        store
            .delete(&blob)
            .await
            .expect("delete of missing key should be Ok");
    }

    #[tokio::test]
    async fn get_missing_returns_not_found() {
        let (store, _dir) = make_store().await;
        let blob = BlobRef {
            backend: "fs".into(),
            key: "does-not-exist".into(),
        };
        let result = store.get(&blob).await;
        assert!(
            matches!(result, Err(AttachmentStoreError::NotFound)),
            "expected NotFound error"
        );
    }

    #[tokio::test]
    async fn path_traversal_keys_rejected() {
        let (store, _dir) = make_store().await;

        let traversal = BlobRef {
            backend: "fs".into(),
            key: "../etc/passwd".into(),
        };
        let hidden = BlobRef {
            backend: "fs".into(),
            key: ".hidden".into(),
        };

        assert!(matches!(
            store.get(&traversal).await,
            Err(AttachmentStoreError::NotFound)
        ));
        assert!(matches!(
            store.get(&hidden).await,
            Err(AttachmentStoreError::NotFound)
        ));
        assert!(matches!(
            store.delete(&traversal).await,
            Err(AttachmentStoreError::NotFound)
        ));
        assert!(matches!(
            store.delete(&hidden).await,
            Err(AttachmentStoreError::NotFound)
        ));
    }

    #[tokio::test]
    async fn put_failure_during_read_does_not_commit_blob() {
        let (store, _dir) = make_store().await;

        // Reader returns 4 bytes then errors — simulates a crash mid-stream.
        let bad_reader: AttachmentReader = Box::pin(ErrorAfter::new(b"part".to_vec()));

        let result = store.put(bad_reader).await;
        assert!(result.is_err(), "put should fail");

        let keys = store.list_keys().await.expect("list_keys");
        assert!(
            keys.is_empty(),
            "no committed blobs expected, found: {keys:?}"
        );
    }

    #[tokio::test]
    async fn put_failure_during_read_does_not_leak_tempfile() {
        let (store, dir) = make_store().await;

        // Reader returns 4 bytes then errors — simulates a crash mid-stream.
        let bad_reader: AttachmentReader = Box::pin(ErrorAfter::new(b"part".to_vec()));

        let result = store.put(bad_reader).await;
        assert!(result.is_err(), "put should fail");

        let tmp_dir = dir.path().join(".tmp");
        let mut read_dir = tokio::fs::read_dir(&tmp_dir).await.expect("read .tmp dir");
        let count = {
            let mut n = 0usize;
            while read_dir.next_entry().await.expect("next entry").is_some() {
                n += 1;
            }
            n
        };
        assert_eq!(count, 0, "tempfile should have been cleaned up inline");
    }

    #[tokio::test]
    async fn list_keys_returns_committed_blobs_only() {
        let (store, dir) = make_store().await;

        let r1 = store.put(reader_from(b"blob one")).await.expect("put 1");
        let r2 = store.put(reader_from(b"blob two")).await.expect("put 2");

        // Write stray files that must be excluded.
        tokio::fs::write(dir.path().join(".tmp").join("stray-tempfile"), b"tmp")
            .await
            .expect("write stray tmp");
        tokio::fs::write(dir.path().join(".hiddenfile"), b"hidden")
            .await
            .expect("write hidden");

        let mut keys = store.list_keys().await.expect("list_keys");
        keys.sort();
        let mut expected = vec![r1.blob.key.clone(), r2.blob.key.clone()];
        expected.sort();
        assert_eq!(keys, expected);
    }

    #[tokio::test]
    async fn cleanup_temp_removes_old_tempfiles() {
        let (store, _dir) = make_store().await;
        let tmp_path = store.tmp_dir().join("old-stale-tempfile");
        tokio::fs::write(&tmp_path, b"stale")
            .await
            .expect("write stale");

        // Duration::ZERO sweeps everything regardless of mtime.
        let removed = store.cleanup_temp(Duration::ZERO).await.expect("cleanup");
        assert_eq!(removed, 1);
        assert!(!tmp_path.exists(), "stale tempfile should be gone");
    }

    #[tokio::test]
    async fn concurrent_puts_produce_distinct_keys() {
        let (store, _dir) = make_store().await;

        let handles: Vec<_> = (0u8..16)
            .map(|i| {
                let s = store.clone();
                tokio::spawn(async move { s.put(reader_from(&[i; 64])).await.expect("put") })
            })
            .collect();

        let mut keys = Vec::new();
        for h in handles {
            let result = h.await.expect("join");
            keys.push(result.blob.key);
        }

        // All keys must be distinct.
        let unique: std::collections::HashSet<_> = keys.iter().collect();
        assert_eq!(unique.len(), 16, "expected 16 distinct keys, got: {keys:?}");

        // All committed blobs must be readable.
        for key in &keys {
            let blob = BlobRef {
                backend: "fs".into(),
                key: key.clone(),
            };
            store.get(&blob).await.expect("get");
        }
    }
}
