use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use catapulte_domain::entity::email::EmailId;
use catapulte_domain::entity::envelope::Envelope;
use catapulte_domain::port::clock::{Clock, SystemClock};
use catapulte_domain::port::email_queue::{
    AckToken, DequeuedEmail, EmailQueue, EmailQueueError, EnqueueOutcome, TraceCarrier,
};

type QueueItem = (EmailId, Envelope, u32, TraceCarrier);
type InFlight = Arc<Mutex<HashMap<u64, QueueItem>>>;
type RxGuard = Arc<tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<QueueItem>>>;

pub struct MemoryQueue<C: Clock = SystemClock> {
    tx: tokio::sync::mpsc::UnboundedSender<QueueItem>,
    rx: RxGuard,
    pending: InFlight,
    next_token: Arc<AtomicU64>,
    ready_count: Arc<AtomicU64>,
    clock: Arc<C>,
}

impl<C: Clock> Clone for MemoryQueue<C> {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
            rx: Arc::clone(&self.rx),
            pending: Arc::clone(&self.pending),
            next_token: Arc::clone(&self.next_token),
            ready_count: Arc::clone(&self.ready_count),
            clock: Arc::clone(&self.clock),
        }
    }
}

impl MemoryQueue {
    #[must_use]
    pub fn new() -> Self {
        Self::with_clock(SystemClock)
    }
}

impl<C: Clock> MemoryQueue<C> {
    /// Builds a queue that reads `clock` to decide how long to hold a scheduled email.
    #[must_use]
    pub fn with_clock(clock: C) -> Self {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        Self {
            tx,
            rx: Arc::new(tokio::sync::Mutex::new(rx)),
            pending: Arc::new(Mutex::new(HashMap::new())),
            next_token: Arc::new(AtomicU64::new(0)),
            ready_count: Arc::new(AtomicU64::new(0)),
            clock: Arc::new(clock),
        }
    }

    /// Makes `item` available after `delay`. It is counted as pending only once it is sent.
    fn send_later(&self, item: QueueItem, delay: Duration) {
        let tx = self.tx.clone();
        let ready_count = Arc::clone(&self.ready_count);
        let due = tokio::time::Instant::now() + delay;
        tokio::spawn(async move {
            tokio::time::sleep_until(due).await;
            // Increment before the send: a concurrent dequeue can only observe the item
            // after it is sent, so the matching decrement never runs first.
            ready_count.fetch_add(1, Ordering::Relaxed);
            if tx.send(item).is_err() {
                ready_count.fetch_sub(1, Ordering::Relaxed);
            }
        });
    }

    /// Returns the number of items waiting in the channel to be dequeued.
    #[must_use]
    pub fn pending(&self) -> u64 {
        self.ready_count.load(Ordering::Relaxed)
    }
}

impl Default for MemoryQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl<C: Clock> EmailQueue for MemoryQueue<C> {
    async fn enqueue(
        &self,
        id: EmailId,
        envelope: &Envelope,
    ) -> Result<EnqueueOutcome, EmailQueueError> {
        let pairs = catapulte_telemetry::propagation::inject_current();
        let trace = TraceCarrier::new(pairs);
        if let Some(send_at_ms) = envelope.send_at_ms {
            let wait_ms = send_at_ms.saturating_sub(self.clock.now_ms());
            if wait_ms > 0 {
                self.send_later(
                    (id, envelope.clone(), 1, trace),
                    Duration::from_millis(wait_ms.unsigned_abs()),
                );
                return Ok(EnqueueOutcome::Published);
            }
        }
        // Increment before the send: a concurrent dequeue can only observe the
        // item after it is sent, so the matching decrement can never run before
        // this increment (which would underflow the counter). Roll back if the
        // send fails.
        self.ready_count.fetch_add(1, Ordering::Relaxed);
        if self.tx.send((id, envelope.clone(), 1, trace)).is_err() {
            self.ready_count.fetch_sub(1, Ordering::Relaxed);
            return Err(EmailQueueError::Storage {
                source: anyhow::anyhow!("memory queue channel closed"),
            });
        }
        Ok(EnqueueOutcome::Published)
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
        self.ready_count.fetch_sub(1, Ordering::Relaxed);

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
            self.send_later((id, envelope, attempt + 1, trace), delay);
        }
        Ok(())
    }

    async fn in_progress(&self, _token: &AckToken) -> Result<(), EmailQueueError> {
        Ok(())
    }

    fn renew_interval(&self) -> Duration {
        Duration::from_secs(1)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use catapulte_domain::entity::body::{BodySource, Plain};
    use catapulte_domain::entity::email::{EmailId, RecipientKind};
    use catapulte_domain::entity::envelope::Envelope;
    use catapulte_domain::port::email_queue::EmailQueue;

    use super::MemoryQueue;

    fn sample_envelope() -> Envelope {
        Envelope::new(
            "sender@example.com",
            vec![(RecipientKind::To, "to@example.com".to_owned())],
            BodySource::Plain(Plain::try_new(Some("hello".to_owned()), None).unwrap()),
        )
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

    #[tokio::test]
    async fn pending_reflects_channel_depth() {
        let queue = MemoryQueue::new();
        assert_eq!(queue.pending(), 0);

        queue
            .enqueue(EmailId::default(), &sample_envelope())
            .await
            .unwrap();
        queue
            .enqueue(EmailId::default(), &sample_envelope())
            .await
            .unwrap();
        assert_eq!(queue.pending(), 2);

        let dequeued = queue.dequeue().await.unwrap();
        assert_eq!(queue.pending(), 1);

        queue.ack(dequeued.token).await.unwrap();
        assert_eq!(queue.pending(), 1);
    }

    struct FixedClock(i64);

    impl catapulte_domain::port::clock::Clock for FixedClock {
        fn now_ms(&self) -> i64 {
            self.0
        }
    }

    async fn settle() {
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
    }

    const NOW_MS: i64 = 1_800_000_000_000;

    fn scheduled(offset_ms: i64) -> Envelope {
        Envelope {
            send_at_ms: Some(NOW_MS + offset_ms),
            ..sample_envelope()
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_future_email_is_held_until_due_then_arrives_with_attempt_one() {
        let queue = MemoryQueue::with_clock(FixedClock(NOW_MS));
        let id = EmailId::default();
        queue.enqueue(id, &scheduled(10_000)).await.unwrap();

        tokio::time::advance(Duration::from_secs(9)).await;
        settle().await;
        assert_eq!(queue.pending(), 0);
        let early = tokio::time::timeout(Duration::from_millis(500), queue.dequeue()).await;
        assert!(early.is_err(), "not available before its time");

        tokio::time::advance(Duration::from_millis(500)).await;
        settle().await;
        assert_eq!(queue.pending(), 1);
        let dequeued = queue.dequeue().await.unwrap();
        assert_eq!(dequeued.id, id);
        assert_eq!(dequeued.attempt, 1);
        assert_eq!(queue.pending(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn a_past_or_current_send_time_is_available_immediately() {
        let queue = MemoryQueue::with_clock(FixedClock(NOW_MS));
        queue
            .enqueue(EmailId::default(), &scheduled(-5_000))
            .await
            .unwrap();
        queue
            .enqueue(EmailId::default(), &scheduled(0))
            .await
            .unwrap();

        assert_eq!(queue.pending(), 2);
        assert_eq!(queue.dequeue().await.unwrap().attempt, 1);
        assert_eq!(queue.dequeue().await.unwrap().attempt, 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_nack_of_a_scheduled_email_still_increments_the_attempt() {
        let queue = MemoryQueue::with_clock(FixedClock(NOW_MS));
        queue
            .enqueue(EmailId::default(), &scheduled(1_000))
            .await
            .unwrap();
        tokio::time::advance(Duration::from_secs(1)).await;
        let first = queue.dequeue().await.unwrap();

        queue
            .nack(first.token, Duration::from_secs(5))
            .await
            .unwrap();
        tokio::time::advance(Duration::from_secs(4)).await;
        settle().await;
        assert_eq!(queue.pending(), 0);
        tokio::time::advance(Duration::from_secs(1)).await;

        assert_eq!(queue.dequeue().await.unwrap().attempt, 2);
    }
}
