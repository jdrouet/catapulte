use std::sync::Arc;
use std::time::Duration;

use catapulte_domain::entity::email::EmailId;
use catapulte_domain::entity::envelope::Envelope;
use catapulte_domain::port::email_queue::{AckToken, EmailQueue, EmailQueueError};
use tokio::sync::mpsc;

struct MemoryQueueInner {
    sender: mpsc::UnboundedSender<(EmailId, Envelope)>,
    receiver: tokio::sync::Mutex<mpsc::UnboundedReceiver<(EmailId, Envelope)>>,
}

#[derive(Clone)]
pub struct MemoryQueue {
    inner: Arc<MemoryQueueInner>,
}

impl MemoryQueue {
    #[must_use]
    pub fn new() -> Self {
        let (sender, receiver) = mpsc::unbounded_channel();
        Self {
            inner: Arc::new(MemoryQueueInner {
                sender,
                receiver: tokio::sync::Mutex::new(receiver),
            }),
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
        self.inner
            .sender
            .send((id, envelope.clone()))
            .map_err(|_| EmailQueueError::Storage {
                source: anyhow::anyhow!("memory queue channel closed"),
            })
    }

    async fn dequeue(&self) -> Result<(EmailId, Envelope, u32, AckToken), EmailQueueError> {
        self.inner
            .receiver
            .lock()
            .await
            .recv()
            .await
            .map(|(id, env)| (id, env, 1u32, AckToken::new(vec![])))
            .ok_or_else(|| EmailQueueError::Storage {
                source: anyhow::anyhow!("memory queue channel closed"),
            })
    }

    async fn ack(&self, _token: AckToken) -> Result<(), EmailQueueError> {
        Ok(())
    }

    async fn nack(&self, _token: AckToken, _delay: Duration) -> Result<(), EmailQueueError> {
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
            subject: None,
            sender: "sender@example.com".to_owned(),
            recipients: vec![(RecipientKind::To, "to@example.com".to_owned())],
            body: BodySource::Plain(Plain::try_new(Some("hello".to_owned()), None).unwrap()),
            variables: serde_json::Map::new(),
        }
    }

    #[tokio::test]
    async fn enqueue_then_dequeue_returns_same_id() {
        let queue = MemoryQueue::new();
        let id = EmailId::default();
        queue.enqueue(id, &sample_envelope()).await.unwrap();
        let (returned_id, _, _, _token) = queue.dequeue().await.unwrap();
        assert_eq!(returned_id, id);
    }

    #[tokio::test]
    async fn ack_is_noop() {
        use catapulte_domain::port::email_queue::AckToken;
        let queue = MemoryQueue::new();
        assert!(queue.ack(AckToken::new(vec![])).await.is_ok());
    }

    #[tokio::test]
    async fn nack_is_noop() {
        use catapulte_domain::port::email_queue::AckToken;
        let queue = MemoryQueue::new();
        assert!(
            queue
                .nack(AckToken::new(vec![]), std::time::Duration::from_secs(1))
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn enqueue_multiple_dequeues_in_order() {
        let queue = MemoryQueue::new();
        let id1 = EmailId::default();
        let id2 = EmailId::default();
        queue.enqueue(id1, &sample_envelope()).await.unwrap();
        queue.enqueue(id2, &sample_envelope()).await.unwrap();
        let (r1, _, _, _) = queue.dequeue().await.unwrap();
        let (r2, _, _, _) = queue.dequeue().await.unwrap();
        assert_eq!(r1, id1);
        assert_eq!(r2, id2);
    }
}
