---
status: proposed
date: 2026-11-11
decision-makers: Jeremie Drouet
---

# Scheduled sends on the storage queue

## Context and Problem Statement

Clients want to submit an email now and have it delivered later (#793), and to cancel it
before it goes out. Today every accepted email is queued for immediate delivery. There is no
delay, and no way to withdraw an email once it is queued.

The feature must hold across restarts, work with several instances sharing one database, and
never deliver an email after a successful cancel. Catapulte offers three queue backends
(memory, NATS JetStream, and a SQLite or PostgreSQL table), and only some of them can hold a
message back until a given time.

## Decision Drivers

- A scheduled email must not be delivered before its time, on any instance.
- After a cancel returns success, the email must never be sent.
- The enqueue step must be safe under concurrent submits, retries and recovery.
- Claiming due work must stay cheap when many rows are scheduled far ahead.
- No new service, no second state machine, and no change to deployments that do not use the
  feature.
- Delivery stays at least once, as today.

## Considered Options

1. **Availability time on the storage queue (chosen).** Each queue row carries the time from
   which it may be claimed. A future `send_at_ms` sets that time.
2. **A scheduler plus a dispatcher.** A separate component holds scheduled emails and moves
   them to the real queue when due. Rejected: it adds a second state machine, a lease to
   elect the dispatcher, and a handoff to NATS that cannot be made transactional with the
   storage update.
3. **Native JetStream message scheduling (ADR-51).** Deferred: it needs NATS 2.12 or later
   while the compose files pin 2.10, it requires stream changes, and its behavior is not yet
   verified for this use. It can be added later as a second capability.

## Decision Outcome

Chosen option: availability time on the storage queue, together with a once-only enqueue and
a cancel guarded by the same row locks. The subsections below record each decision.

### (a) Where the delay lives, and which deployments support it

The storage queue (SQLite or PostgreSQL) holds scheduled emails. A queue row is claimable only
when its availability time has passed. Support depends on the queue adapter, not on the
storage backend: a deployment with `QueueAdapter::Sqlite` or `QueueAdapter::Postgres`
supports scheduling. NATS and memory queue deployments reject `send_at_ms` with 422. NATS
support is left to a later decision on JetStream schedules.

### (b) Durable, once-only enqueue

Storage `enqueue` becomes one transaction:

1. `UPDATE emails SET enqueue_state = 'enqueued' WHERE id = ? AND api_key = ? AND
   enqueue_state <> 'enqueued' AND cancelled_at_ms IS NULL`. The cancelled predicate arrives
   with cancellation.
2. If no row changed, check in the same transaction that the email exists for this owner.
   If it does, commit and report that it was already enqueued; insert nothing. If it does
   not, fail with a storage error, so a missing email is never mistaken for an enqueued one.
3. Otherwise check whether a queue row already exists for the email. A row left by an older
   version is adopted: nothing is inserted and the state is now `enqueued`. If none exists,
   insert the queue row with `claimed_until = send_at_ms.unwrap_or(now)`.
4. Commit.

The `UPDATE` is both the lock and the compare-and-set. PostgreSQL re-evaluates it after a
concurrent commit, and SQLite serialises writers. The decision lives on the `emails` row, so
it survives `ack` deleting the queue row: an email submitted with this version gets at most
one queue row for its whole life. This also closes an existing gap where a replayed enqueue
could insert a second row after the first was acknowledged. The guarantee holds for new
submissions only: the history of emails queued by an older version cannot be established
afterwards, which is why an existing row is adopted instead of duplicated.

Lock order is `emails` first, then `email_queue`, for both enqueue and cancel. Dequeue locks
only the queue row and reads `emails` without a lock, so no cycle exists.

`enqueue` reports `Inserted` (this call made the email available, by inserting or adopting a
row) or `AlreadyEnqueued` (an earlier call did). The submit path records the enqueued state
and publishes the `queued` or `scheduled` event only for `Inserted`. A late call that loses the
race, for example a paused submit that resumes after a recovery was delivered and
acknowledged, therefore adds no row, no state write and no event, and cannot put a `queued`
event after a terminal one.

An error from `COMMIT` itself is reported as uncertain, because the outcome is unknown and the
caller must treat the email as possibly queued. Errors before the commit are definite
failures: the transaction rolled back and nothing was queued.

NATS and memory queues are unchanged. Delivery stays at least once, and the only duplicate
path is the existing 300 s claim expiry.

### (c) Cancel

`POST /emails/{id}/cancel` is scoped to the caller through the existing read scope; an admin
key can cancel any email. One transaction does the work:

1. ```sql
   UPDATE emails SET cancelled_at_ms = :now
   WHERE id = :id AND (:owner IS NULL OR api_key = :owner)
     AND cancelled_at_ms IS NULL
     AND send_at_ms IS NOT NULL AND enqueue_state = 'enqueued'
     AND NOT EXISTS (SELECT 1 FROM lifecycle_events
                     WHERE email_id = emails.id AND event_type IN (worker event types))
   RETURNING correlation_id, tags, extras
   ```
   When no row is updated, a scoped `SELECT cancelled_at_ms` classifies the result: no row
   means `NotFound`, a set value means `AlreadyCancelled`, anything else means
   `NotCancellable`.
2. `DELETE FROM email_queue WHERE email_id = ? AND attempt_count = 0`, giving n rows.
3. Check whether any queue row for the email remains.
4. If n is 1 and nothing remains, commit and return `Cancelled` with the event context.
   Otherwise roll back and return `NotCancellable`, which also undoes `cancelled_at_ms`.

**Guarantee.** After a 204, no worker has claimed the email, no queue row remains, and none
can be inserted: the enqueue compare-and-set refuses it, and `resolve_duplicate` short-circuits
cancelled rows.

**Concurrency.** On PostgreSQL, the step 2 `DELETE` waits on a row locked by an in-flight
claim. After that claim commits, `attempt_count` is 1, the row is not deleted, and step 4 rolls
back. Claims skip locked rows. On SQLite the first statement is a write, so the transaction is
serialised.

**Legacy rows.** Emails without `send_at_ms`, which includes every email created before this
feature, are never cancellable. The worker-event check and the exactly-one-row check guard an
overlap during a rolling upgrade. A residual risk remains if that overlap coincides with
failed event writes. Operators must upgrade every instance before clients use `send_at_ms`.

**Responses.** 204 for `Cancelled` and `AlreadyCancelled` (the second publishes no event); 404
for an unknown or foreign id; 409 with `{"error":"email can no longer be cancelled"}`; 400 for a
malformed id.

### (d) Events and status

- `scheduled{send_at_ms}` replaces `queued` when `send_at_ms` is later than now at announce
  time. `cancelled{}` is published after the commit. Both are best effort, and neither belongs
  to the terminal, pipeline or worker event lists.
- The `cancelled_at_ms` column is the authority for cancellation. This is a deliberate
  exception to the rule that status derives from events, because the fence must be atomic
  with the queue delete.
- Status is computed in this order: `cancelled_at_ms` set gives `cancelled`; the latest
  terminal `delivery.succeeded` gives `sent`; `delivery.failed` gives `failed`; `send_at_ms`
  later than now gives `scheduled`; otherwise `queued`.
- A `status=queued` filter no longer matches scheduled or cancelled emails.
- The HTTP list maps the new statuses explicitly: records report `scheduled` and `cancelled`
  as their `status`, `status=scheduled` and `status=cancelled` are accepted filters, and a
  scheduled record also carries `send_at_ms`.

### (e) Idempotency

- The same key while the email is scheduled returns the original id. The first write wins and
  no event is published.
- The same key after a cancel returns the original id with status `cancelled`.
  `resolve_duplicate` returns `(Complete, None)` when `cancelled_at_ms` is set. A client that
  wants to send again must use a new key.
- **Recovery.** Before every recovery enqueue, the stored `send_at_ms` is checked. A future
  time on a queue that cannot schedule yields `SchedulingUnavailable`, writes nothing, and
  leaves the row publishable. An overdue time is enqueued without re-checking tolerance or
  horizon.
- **Late retries.** `send_at_ms` is validated before the idempotency lookup. A retry that
  repeats the original `send_at_ms` more than the past tolerance after it has passed gets a
  400 (`SendAtOutOfRange`). The client recovers the email by retrying without `send_at_ms`,
  which returns the original id and republishes the stored envelope if needed.

### (f) Limits and clock

- `SEND_AT_PAST_TOLERANCE_MS` is a constant of 60,000. A time within the tolerance in the past
  is sent immediately with a `queued` event; an older time is rejected.
- The maximum horizon comes from `CATAPULTE_MAX_SCHEDULE_AHEAD_SECS`, default 2,592,000
  (30 days). A later time is rejected with 400. Arithmetic saturates.
- The reference clock is application time (`now_ms()` on the acting node) for validation,
  availability, claims, nacks, status and `cancelled_at_ms`. The guarantee is "not before
  `send_at` on the claiming node's clock", and synchronised clocks (NTP) are an operational
  assumption. The PostgreSQL column default, which uses the database clock, applies only to
  old writers during a rolling upgrade.

### (g) Multiple instances

There is no dispatcher. Instances claim with `SKIP LOCKED`. Enqueue and cancel are serialised
by the `emails` row lock. SQLite remains single-instance.

### (h) Garbage collection

- A scheduled email is live. With no pipeline event yet, the existing `queued` fallback keeps
  its blobs.
- A cancelled email is excluded from liveness.
- The grace period is measured on blob age, so the blobs of an old scheduled email can be
  deleted on the first sweep after its cancel. This is documented.
- Object-store expiry must be longer than the maximum horizon.

### (i) Claim performance

Every queue row has an availability time in `claimed_until`. New rows get `send_at_ms` or the
current time. The migration makes a `NULL` availability time impossible to store:

- On PostgreSQL it installs the column default first, then backfills existing `NULL` rows from
  `enqueued_at`, then sets the column `NOT NULL`. Older writers that omit the column get the
  default, and an explicit `NULL` fails instead of stranding a row.
- On SQLite, which cannot alter a column, it backfills the same way and adds a trigger that
  fills a `NULL` availability time on insert.

The backfill rewrites rows and locks the queue table while it runs. The queue is normally
small, but operators should apply the migration in a maintenance window.

The claim becomes `WHERE claimed_until <= ? ORDER BY claimed_until LIMIT 1` and uses the
existing `claimed_until` index, so rows scheduled far ahead are not scanned. The pending count
uses the same predicate.

Ordering becomes earliest-availability ordering: the row with the earliest availability time is
claimed first, and ties are unspecified. Rows that are due immediately keep their order;
retried rows are now ordered by their retry time.

Budget: with 100,000 future rows, the query plan shows an index range scan, and on SQLite 100
empty claims complete in under 100 ms.

## Consequences

- Scheduling works only on the SQLite and PostgreSQL queues. NATS and memory deployments get a
  clear 422.
- Each email submitted with this version has at most one queue row, and the replay duplicate
  is gone. Rows left by older versions are adopted, not duplicated.
- PostgreSQL enqueue costs one extra round trip.
- Queue ordering changes to earliest-availability ordering. The readme notes it.
- Templates, suppressions and quotas are evaluated at send time, not at submit time.
- Attachments must outlive the delay, so object-store expiry must exceed the horizon.
- A cancel is final for an idempotency key; sending again needs a new key.
- Cancelling is limited to emails submitted with `send_at_ms` after the upgrade.
- The queue migration locks the queue table while it backfills, so it needs a maintenance
  window.
- If an operator switches from a storage queue to NATS or memory, rows for future times stay
  publishable, and recovery reports `SchedulingUnavailable`. Over NATS this is acknowledged
  and discarded.
- Status now depends on a column and on the clock, not only on events.

## Confirmation

- Adapter tests on both backends cover: a deterministic paused-submit scenario that must not
  produce a queue row; a second enqueue inserting nothing; concurrent enqueues giving one row;
  `send_at_ms` round-tripping through save, dequeue and recovery, adoption of a legacy queue
  row, a failed insert reporting a definite failure, the commit error mapping, and a late
  enqueue after a terminal event adding nothing and leaving the blobs not live. A pg test holds
  the email row lock and checks that a blocked enqueue sees the committed state.
- Cancel tests cover each outcome (cancelled, already cancelled, claimed, delivered, no
  `send_at_ms`, foreign, admin, unknown), the legacy multi-row cases with rollback, and a
  repeated race of cancel against dequeue where exactly one side wins.
- Query plan tests assert an index range scan with 100,000 future rows on both backends, and a
  coarse SQLite latency check.
- Domain tests with a fixed clock cover tolerance, horizon, saturation, unavailable queues and
  the recovery checks.
- End-to-end scenarios check that a scheduled email arrives no earlier than `send_at_ms`, that
  a cancelled email never arrives, and that NATS and memory deployments reject scheduling.

## More Information

Decision (c), Cancel, is amended by "Scheduled sends on the NATS and memory queues"
(202611121500): its commit rule is relaxed and a delivery fence is added.

The design answers the concerns raised against earlier drafts.

- **A separate scheduler and dispatcher.** Not used. Delay is a property of the queue row, so
  there is no second state machine, no lease, and no non-transactional handoff to NATS.
- **Duplicate queue rows after an acknowledged enqueue.** The `enqueued` decision lives on the
  `emails` row, not on the queue row, so it survives `ack`.
- **Cancel racing a worker claim.** Cancel deletes only rows with `attempt_count = 0` and rolls
  back unless exactly one row was removed and none remain. A concurrent claim always wins or
  loses cleanly.
- **A cancelled email returning through retries.** The enqueue compare-and-set and
  `resolve_duplicate` both refuse cancelled rows.
- **Recovery on a queue that cannot schedule.** Recovery checks the stored time first and
  leaves the row untouched.
- **A late `queued` event after a terminal one.** Only an `Inserted` enqueue is announced, so a
  paused submit that resumes after the email was delivered adds no event.
- **Queue rows left by an older version.** Enqueue adopts an existing row instead of inserting a
  second one. Once-only holds for new submissions; older history cannot be reconstructed.
- **A client replaying an expired schedule.** Validation runs before the idempotency lookup, so
  the retry gets a 400; omitting `send_at_ms` recovers the original email.
- **Cost of claiming with many future rows.** Availability time is in an indexed column, and
  the plan is checked by tests.
- **Clock differences between the database and nodes.** One reference clock, the application
  clock, is used everywhere except for old writers during a rolling upgrade.
