use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use catapulte_domain::entity::email::EmailId;
use catapulte_domain::entity::envelope::Envelope;
use catapulte_domain::port::email_queue::{
    AckToken, DequeuedEmail, EmailQueue, EmailQueueError, TraceCarrier,
};

type QueueItem = (EmailId, Envelope, u32, TraceCarrier);
type InFlight = Arc<Mutex<HashMap<u64, QueueItem>>>;
type RxGuard = Arc<tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<QueueItem>>>;

#[derive(Clone)]
pub struct MemoryQueue {
    tx: tokio::sync::mpsc::UnboundedSender<QueueItem>,
    rx: RxGuard,
    pending: InFlight,
    next_token: Arc<AtomicU64>,
}

impl MemoryQueue {
    #[must_use]
    pub fn new() -> Self {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        Self {
            tx,
            rx: Arc::new(tokio::sync::Mutex::new(rx)),
            pending: Arc::new(Mutex::new(HashMap::new())),
            next_token: Arc::new(AtomicU64::new(0)),
        }
    }
}

impl Default for MemoryQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl EmailQueue for MemoryQueue {
    async fn enqueue(&self, id: EmailId, envelope: &Envelope) -> Result<(), EmailQueueError> {
        let pairs = catapulte_telemetry::propagation::inject_current();
        let trace = TraceCarrier::new(pairs);
        self.tx
            .send((id, envelope.clone(), 1, trace))
            .map_err(|_| EmailQueueError::Storage {
                source: anyhow::anyhow!("memory queue channel closed"),
            })
    }

    async fn dequeue(&self) -> Result<DequeuedEmail, EmailQueueError> {
        let (id, envelope, attempt, trace) =
            self.rx
                .lock()
                .await
                .recv()
                .await
                .ok_or_else(|| EmailQueueError::Storage {
                    source: anyhow::anyhow!("memory queue channel closed"),
                })?;

        let token_id = self.next_token.fetch_add(1, Ordering::Relaxed);
        let token = AckToken::new(token_id.to_le_bytes().to_vec());
        self.pending
            .lock()
            .unwrap()
            .insert(token_id, (id, envelope.clone(), attempt, trace.clone()));
        Ok(DequeuedEmail {
            id,
            envelope,
            attempt,
            token,
            trace,
        })
    }

    async fn ack(&self, token: AckToken) -> Result<(), EmailQueueError> {
        let bytes: [u8; 8] = token.0.try_into().map_err(|_| EmailQueueError::Storage {
            source: anyhow::anyhow!("invalid ack token length"),
        })?;
        let token_id = u64::from_le_bytes(bytes);
        self.pending.lock().unwrap().remove(&token_id);
        Ok(())
    }

    async fn nack(&self, token: AckToken, delay: Duration) -> Result<(), EmailQueueError> {
        let bytes: [u8; 8] = token.0.try_into().map_err(|_| EmailQueueError::Storage {
            source: anyhow::anyhow!("invalid nack token length"),
        })?;
        let token_id = u64::from_le_bytes(bytes);
        let entry = self.pending.lock().unwrap().remove(&token_id);
        if let Some((id, envelope, attempt, trace)) = entry {
            let tx = self.tx.clone();
            tokio::spawn(async move {
                tokio::time::sleep(delay).await;
                let _ = tx.send((id, envelope, attempt + 1, trace));
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use catapulte_domain::entity::body::{BodySource, Plain};
    use catapulte_domain::entity::email::{EmailId, RecipientKind};
    use catapulte_domain::entity::envelope::Envelope;
    use catapulte_domain::port::email_queue::EmailQueue;

    use super::MemoryQueue;

    fn sample_envelope() -> Envelope {
        Envelope {
            idempotency_key: None,
            correlation_id: None,
            subject: None,
            sender: "sender@example.com".to_owned(),
            recipients: vec![(RecipientKind::To, "to@example.com".to_owned())],
            body: BodySource::Plain(Plain::try_new(Some("hello".to_owned()), None).unwrap()),
            variables: serde_json::Map::new(),
            attachments: vec![],
        }
    }

    #[tokio::test]
    async fn enqueue_then_dequeue_returns_same_id() {
        let queue = MemoryQueue::new();
        let id = EmailId::default();
        queue.enqueue(id, &sample_envelope()).await.unwrap();
        let dequeued = queue.dequeue().await.unwrap();
        assert_eq!(dequeued.id, id);
    }

    #[tokio::test]
    async fn enqueue_multiple_dequeues_in_order() {
        let queue = MemoryQueue::new();
        let id1 = EmailId::default();
        let id2 = EmailId::default();
        queue.enqueue(id1, &sample_envelope()).await.unwrap();
        queue.enqueue(id2, &sample_envelope()).await.unwrap();
        let r1 = queue.dequeue().await.unwrap();
        let r2 = queue.dequeue().await.unwrap();
        assert_eq!(r1.id, id1);
        assert_eq!(r2.id, id2);
    }

    #[tokio::test]
    async fn ack_removes_item_from_pending() {
        let queue = MemoryQueue::new();
        let id = EmailId::default();
        queue.enqueue(id, &sample_envelope()).await.unwrap();
        let dequeued = queue.dequeue().await.unwrap();

        queue.ack(dequeued.token).await.unwrap();

        assert!(queue.pending.lock().unwrap().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn nack_requeues_item_after_delay() {
        let queue = MemoryQueue::new();
        let id = EmailId::default();
        queue.enqueue(id, &sample_envelope()).await.unwrap();
        let dequeued = queue.dequeue().await.unwrap();
        assert_eq!(dequeued.id, id);
        assert_eq!(dequeued.attempt, 1);

        queue
            .nack(dequeued.token, std::time::Duration::from_millis(100))
            .await
            .unwrap();

        // Advance virtual time past the nack delay; the spawned sleep task fires and
        // sends the item back to the channel.
        tokio::time::advance(std::time::Duration::from_millis(200)).await;

        let dequeued2 = queue.dequeue().await.unwrap();
        assert_eq!(dequeued2.id, id);
        assert_eq!(dequeued2.attempt, 2);
    }
}
