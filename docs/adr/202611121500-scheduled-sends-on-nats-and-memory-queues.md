---
status: proposed
date: 2026-11-12
decision-makers: Jeremie Drouet
---

# Scheduled sends on the NATS and memory queues

## Context and Problem Statement

The storage queue supports scheduled sends and cancellation (see "Scheduled sends on the
storage queue"). NATS and memory deployments reject `send_at_ms` with 422 (#825). We want the
same feature on both, without changing what clients see: a scheduled email is never delivered
before its time, and never delivered after a successful cancel.

Two facts shape the design. The emails repository is always SQLite or PostgreSQL, whatever
the queue, so the `emails` row is available as a decision point for every queue. And NATS and
memory queues have no queue row that cancel can delete, so the storage cancel rule, which
relies on that row, does not carry over.

## Decision Drivers

- A scheduled email must not be delivered before its time, on any instance or queue.
- After a cancel returns success, the email must never be sent, including when the queue has
  already released the message.
- The enqueue step stays safe under concurrent submits, retries and recovery.
- No new service and no second state machine.
- A deployment must never publish a future email for immediate delivery because the queue
  cannot hold it back.
- Delivery stays at least once, as today.

## Considered Options

1. **JetStream message schedules plus a delivery fence (chosen).** NATS holds the message
   until its time with a native `@at` schedule. A fence on the `emails` row settles the race
   between cancel and delivery. Memory uses a delayed in-process send.
2. **`nak_with_delay` until the email is due.** Rejected: each redelivery consumes
   `max_deliver` and increases the attempt count, so a long delay exhausts the retry budget.
3. **A separate schedule stream and a dispatcher.** Rejected for the reason given in the
   earlier ADR: a second state machine and a handoff that cannot be made transactional.
4. **Purge the schedule on cancel.** Rejected: it does not cover the window where the schedule
   has fired and the worker has not yet picked the message up. The fence covers both, so a
   purge would add a second mechanism without removing the first.
5. **Fail startup on a NATS server older than 2.12.** Rejected: a deployment that never uses
   scheduling would stop working after an upgrade. Such a deployment starts, logs a warning,
   and answers 422 to `send_at_ms`.
6. **A min-heap for the memory queue.** Rejected: one delayed task per scheduled email is
   enough, and the memory queue loses its content on restart anyway.
7. **A capped wait in the worker with a nack fallback.** Rejected: the nack burns a delivery
   attempt and can lose the email when `max_deliver` is low. The worker waits in place
   instead.

## Decision Outcome

Chosen option: JetStream `@at` schedules and a delivery fence on the `emails` row, with a
worker guard that never processes an email early. The subsections below record each decision.

### (a) Delivery fence on the email row

A new nullable column `delivery_started_at_ms` on `emails` records when a worker first picked
the email up. It applies to every queue.

When a worker receives an envelope that has `send_at_ms`, it runs
`UPDATE emails SET delivery_started_at_ms = COALESCE(delivery_started_at_ms, now) WHERE id = ?
AND cancelled_at_ms IS NULL` before any side effect:

- One row updated: proceed.
- No row updated and the email is cancelled: acknowledge and drop the message, with no events.
- No row updated and the email is missing: proceed, as today.

Cancel gains the predicate `delivery_started_at_ms IS NULL`. Both sides write the same
`emails` row, so exactly one wins: if the worker commits first, cancel reports
`NotCancellable`; if cancel commits first, the worker drops the message.

On NATS and memory, a 204 from cancel leaves the queue message in place. It fires at its time,
the worker reaches the fence, finds the cancellation and acknowledges it.

The storage cancel commit rule relaxes from "exactly one queue row removed and none remain" to
"at most one removed and none remain". Zero removed is valid when the queue has no row for the
email, which is always the case on NATS and memory. The race with a storage claim is still
decided as before: a claimed row has `attempt_count` 1 and is not deleted, and the fence blocks
cancel once the worker has recorded the pickup.

Recovery evidence: `resolve_duplicate` on SQLite and PostgreSQL treats a set
`delivery_started_at_ms` as proof that a worker saw the email. The row is promoted to
`enqueued` and recovery does not replay it.

### (b) Worker never-early guard

The worker checks `send_at_ms` before the fence, the sending state and any other side effect.
If the email is not yet due on the injected wall clock, the worker waits in place:

- It re-reads the clock on every pass, so a backward jump only lengthens the wait.
- It calls the new port method `in_progress` on the queue immediately, then every renewal
  interval. Each sleep lasts the smaller of the interval and the time remaining.
- It never nacks because the email is early, so no delivery attempt is consumed.
- It logs one `warn!` when the wait exceeds 30 seconds, which indicates clock skew between the
  firing source and the worker.
- If `in_progress` fails, the worker logs the error and stops handling the message. The lease
  lapses and the queue redelivers it.

The renewal interval is one third of the lease. On NATS the lease is the effective ack
deadline, the minimum of `ack_wait` and every backoff entry of the consumer as observed. On
the storage queues it is the claim lease. On the memory queue it is one second.

`in_progress` has one implementation per queue. On NATS it sends a progress acknowledgement,
which resets the ack timer and leaves the delivery unchanged. On the storage queues it extends
the claim with `claimed_until = now + lease` for the claimed row, with the same lease as the
claim, so no other worker can claim the row during a long wait. On the memory queue it does
nothing, because there is no lease.

A wait lasts only as long as the clock skew between the firing source and the worker, or a
backward wall-clock jump. The cost is one worker slot held during that time.

### (c) NATS JetStream `@at` schedules

JetStream message schedules exist from server 2.12. A message published with a
`Nats-Schedule: @at <RFC3339>` header and a `Nats-Schedule-Target` header is stored until its
time, then republished to the target subject by the server. It never fires before its time, a
time in the past fires immediately, and the fired message replaces the schedule.

- **Subject.** A future email is published to `{subject}.scheduled.<id>`, where `<id>` is the
  email id, with the schedule target set to the queued subject. The message id header is the
  email id and the trace headers are kept. The `@at` value is the send time rounded up to the
  next second, in UTC. The envelope now carries `send_at_ms`, which it dropped before.
- **Stream setup.** The adapter gets or creates the stream and the consumer as today, then
  decides readiness from the configuration it observes. Scheduling is supported only if all of
  these hold:
  - the consumer filter is exactly the queued subject;
  - the stream retention is WorkQueue or Limits;
  - the stream has no mirror and no sources;
  - the stream discard policy is Old;
  - the server version is 2.12 or later;
  - the effective ack deadline is at least one second.
- **Stream upgrade.** If the stream lacks the schedule subject or the `allow_msg_schedules`
  flag, the adapter updates it from the configuration it fetched, adding
  `{subject}.scheduled.*` and the flag, so no other field changes. The flag is set
  automatically at startup. It cannot be turned off again once set.
- **Not ready.** If any check fails, the adapter leaves the stream untouched where the check
  concerns the consumer, logs a warning at startup, and the API answers 422 to `send_at_ms`.
  A future email is never published for immediate delivery: enqueueing one without support
  fails with a storage error.

### (d) Memory queue

A future `send_at_ms` makes the queue spawn a task that sleeps until the time and then sends
the item. The queue takes an injected clock, so tests run with paused time. The delayed-send
helper also serves nack and takes the intended attempt: enqueue uses attempt 1 and nack uses
the next attempt. `pending()` excludes items that are not yet due. Scheduled items are lost on
restart, as the rest of the memory queue is.

### (e) Capability per queue

A queue reports the maximum schedule horizon when it supports scheduling, and nothing
otherwise. SQLite, PostgreSQL and memory always support it. NATS supports it only when the
readiness checks in (c) pass. An unsupported queue gives a startup warning and a 422 for
`send_at_ms`.

### (f) Announce winner

On NATS and memory, a successful publish does not say whether this call or an earlier one made
the email available. The submit path needs one winner that announces `queued` or `scheduled`.

- `enqueue` can return a new outcome, `Published`, for NATS and memory. `Inserted` and
  `AlreadyEnqueued` keep their meaning from the earlier ADR.
- `set_enqueue_state` returns whether this call won. Setting `enqueued` is a compare-and-set:
  `UPDATE ... WHERE enqueue_state <> 'enqueued' RETURNING (delivery_started_at_ms IS NULL AND
  NOT EXISTS worker events)`. No row updated for an existing email means it was already set, so
  the result is false; a missing email is an error.
- On `Inserted`, the storage compare-and-set already ran, so the call announces. On
  `AlreadyEnqueued` it announces nothing. On `Published` it writes the state and announces only
  when the write returns true.
- If the state write fails, the error is logged and nothing is announced. A retry with the same
  idempotency key republishes and wins the compare-and-set. A submit without an idempotency
  key has no retry path, so a failed state write drops the `queued` or `scheduled` event, and
  status falls back to `queued`.

The publish acknowledgement is not used to detect duplicates. Mapping a duplicate publish to
`AlreadyEnqueued` would skip the state write after an uncertain publish.

## Consequences

- NATS and memory deployments accept `send_at_ms` and cancel, with the same responses as the
  storage queue. A NATS deployment on a server older than 2.12, or with an unsuitable stream or
  consumer, still answers 422 and logs a startup warning.
- Upgrade notes for NATS: every node of the cluster must run 2.12 or later, because stream
  schedules need that API level, and the check at startup sees only the node it connects to.
  The `allow_msg_schedules` flag is set at startup and cannot be removed. Downgrading the
  server below 2.12 afterwards is not safe.
- Every Catapulte instance must be upgraded before clients use `send_at_ms`, because an older
  worker does not know the fence. The deployment files that pinned NATS 2.10 move to 2.14.
- A cancelled schedule stays in the stream until its send time, and the worker then drops it.
- A worker slot is held during a skew wait.
- The memory queue holds one sleeping task per scheduled email and loses them on restart.
- A replay after the schedule fired and before a worker picked it up, outside the dedup
  window, can deliver twice. This is the existing at-least-once behavior.
- The cluster version check reads one node.
- A failed state write on an unkeyed submit loses the `queued` or `scheduled` event.
- The announcement can still arrive after a worker's terminal event if the submitter pauses
  between the enqueue-state write and publishing the event. The same window exists on the
  storage queue. Status is unaffected, because terminal events take precedence.
- The cancel commit rule on the storage queue is weaker than before. The fence now carries the
  guarantee that the queue row carried alone.

## Confirmation

- Storage tests cover cancel with no queue row, the fence against cancel repeated 50 times with
  exactly one winner, recovery treating `delivery_started_at_ms` as evidence, and the existing
  cancel tests unchanged.
- Recovery tests cover exactly one `queued` event after concurrent submits, a scheduled retry
  before firing, and a committed fence with no events and an expired dedup window producing no
  republish.
- Worker tests with a scripted clock cover an email 10 seconds early (at least 9 `in_progress`
  calls, attempt stays 1), a backward clock jump, a transient failure after the wait, a wait
  over 30 seconds logging one warning without a nack, and an `in_progress` error. A NATS test
  with `max_deliver` 1 and a short `ack_wait` renews for 5 seconds without a redelivery.
- NATS container tests run on the minimum supported server: fresh stream, upgrade preserving
  custom fields, restart, concurrent connect, an unfiltered consumer leaving the stream
  untouched and unsupported, Interest retention unsupported, and the effective ack deadline
  computed from `ack_wait` and backoff and asserted on the returned consumer configuration.
  A server older than 2.12 is unsupported and refuses a future enqueue.
- Memory tests use paused time.
- End-to-end scenarios run scheduling on all six queue and storage combinations.

## More Information

- This ADR amends decision (c) of "Scheduled sends on the storage queue": the cancel commit
  rule and the guarantee now rest on the delivery fence as well as the queue row.
- Native schedules are described in the JetStream design record ADR-51 of the NATS project.
