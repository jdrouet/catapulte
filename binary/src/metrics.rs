use std::sync::Arc;
use std::time::Duration;

use opentelemetry::KeyValue;

use catapulte_domain::use_case::list_senders::ListSendersUseCase as _;

use crate::queue::QueueAdapter;
use crate::state::ListSendersServiceImpl;

pub(crate) async fn run_sampler(
    queue: QueueAdapter,
    list_senders: Arc<ListSendersServiceImpl>,
    backend: &'static str,
    interval: Duration,
    cancel: tokio_util::sync::CancellationToken,
) {
    let meter = opentelemetry::global::meter("catapulte");
    let queue_pending = meter.u64_gauge("catapulte.queue.pending").build();
    let sender_sent = meter.u64_gauge("catapulte.sender.sent_in_range").build();
    let sender_quota_limit = meter.u64_gauge("catapulte.sender.quota_limit").build();

    // Sample once immediately, then on the configured interval.
    sample_once(
        &queue,
        &list_senders,
        backend,
        &queue_pending,
        &sender_sent,
        &sender_quota_limit,
    )
    .await;

    loop {
        tokio::select! {
            () = cancel.cancelled() => break,
            () = tokio::time::sleep(interval) => {}
        }
        sample_once(
            &queue,
            &list_senders,
            backend,
            &queue_pending,
            &sender_sent,
            &sender_quota_limit,
        )
        .await;
    }
}

async fn sample_once(
    queue: &QueueAdapter,
    list_senders: &ListSendersServiceImpl,
    backend: &'static str,
    queue_pending: &opentelemetry::metrics::Gauge<u64>,
    sender_sent: &opentelemetry::metrics::Gauge<u64>,
    sender_quota_limit: &opentelemetry::metrics::Gauge<u64>,
) {
    if let Some(n) = queue.pending().await {
        queue_pending.record(n, &[KeyValue::new("backend", backend)]);
    }

    match list_senders.execute().await {
        Ok(snaps) => {
            for s in snaps {
                let sender = s.config.name.as_str().to_owned();
                sender_sent.record(s.sent_in_range, &[KeyValue::new("sender", sender.clone())]);
                if let Some(q) = &s.config.quota {
                    sender_quota_limit.record(q.count, &[KeyValue::new("sender", sender)]);
                }
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "sender metrics sample failed");
        }
    }
}
