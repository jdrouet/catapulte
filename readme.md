# Catapulte

Make sending email easy.

> **Using the API?** See the [usage guide](./docs/usage.md) for how to submit
> emails (plain, HTML, MJML, attachments, batches), read their state, and
> subscribe to lifecycle events. The rest of this readme covers running and
> configuring a server.

## User stories

Within each persona, stories are ordered by priority (most important first).

### API consumer

- [x] As an API consumer, I can ask an email (text or html) to be sent through a SMTP server, and get back a tracking id, so that I don't have to manage SMTP and retries myself.
- [x] As an API consumer, I can ask an email to be sent from inline mjml plus variables, so that I keep template sources in my own repo.
- [x] As an API consumer, I can ask an email with attachments to be sent through a SMTP server, so that I can send invoices, receipts or reports.
- [x] As an API consumer, I can list emails I previously submitted with filters (status `queued` / `sent` / `failed`, time range, recipient, template, tracking id), paginated, so that I can check delivery state and debug without keeping my own mirror of the data.
- [x] As an API consumer, I can pass an idempotency key on submission, so that retrying a failed request doesn't send the email twice.
- [x] As an API consumer, I can submit a batch of emails in a single request and get back one tracking id per email, so that I can fan out a campaign without N round-trips. Partial acceptance is allowed: per-email validation errors are returned alongside the accepted ids.
- [x] As an API consumer, I can ask an email to be sent from a pre-registered template name + variables, so that callers don't ship template bytes on every request.
- [x] As an API consumer, I can ask an email to be sent from a remote mjml template fetched over http (with `mj-include`) + variables, so that templates can live in a CMS or shared repo.
- [x] As an API consumer, I can list the lifecycle events for emails I submitted (`queued`, `sending`, `delivery.succeeded`, `delivery.failed`, `retrying`), with filters (tracking id, event type, time range), paginated, so that I can debug a delivery without subscribing to the live event stream.

### Operator

- [x] As an operator, I can configure multiple SMTP servers with routing rules, so that I can fail over or split traffic per sender domain.
- [x] As an operator, I can set per-server quotas (rate and daily cap), so that I stay within provider limits without dropping traffic.
- [ ] As an operator, I can list lifecycle events across all submissions (not scoped to one consumer) with filters (event type, time range, upstream server, error class), paginated, so that I can investigate incidents and audit traffic. _(global listing with event-type and time-range filters and pagination is supported; filtering by upstream server and error class is not yet.)_
- [x] As an operator, I can expose multiple ingress transports for API consumers (HTTP for request/response CRUD, NATS for fire-and-forget submissions, more later), so that consumers can pick the integration style that fits their stack. Each transport can be enabled or disabled independently. NATS submissions don't return a tracking id synchronously: the consumer supplies a correlation id and observes outcome via lifecycle events.

### Event subscriber

- [x] As an event subscriber, I receive a `delivery.succeeded` event when an email is accepted by the upstream SMTP, so that I can update my own state.
- [x] As an event subscriber, I receive a `delivery.failed` event after retries are exhausted, so that I can alert or compensate. The event carries the last error and the attempt count.
- [x] As an event subscriber, I receive events over whichever transport the operator has enabled globally (webhook to a configured URL, or NATS on a configured subject), so that I can plug catapulte into the bus my stack already speaks without managing per-subscription transport config.


## Quick Start

The compose files are for local development and testing only; do not deploy them as is. Their default credentials (`catapulte`) can be overridden with `POSTGRES_PASSWORD` and `MINIO_ROOT_PASSWORD` (a Postgres password must be URL-safe: letters, digits, `-._~`, because it is embedded in `CATAPULTE_POSTGRES_URL`), and the MinIO and Redis ports are published on `127.0.0.1` only.

The easiest way to run Catapulte is using Docker Compose. Several examples are provided in the [compose](./compose) directory:

- **Local Development**: `docker-compose -f compose/local-dev.yml up`
  Starts Catapulte with an in-memory database and [Mailpit](https://github.com/axllent/mailpit) for local SMTP testing.
- **SQLite (Persistent)**: `docker-compose -f compose/sqlite.yml up`
  Starts Catapulte with a persistent SQLite database.
- **Postgres & NATS**: `docker-compose -f compose/postgres-nats.yml up`
  A more robust setup using Postgres for storage and NATS for the email queue and events.
- **S3 (RustFS)**: `docker-compose -f compose/minio.yml up`
  Runs Catapulte with a local [RustFS](https://rustfs.com) instance as the S3-compatible attachment backend. (The file and its `minio` service names are unchanged; the MinIO images are no longer published.)
- **Redis (attachments)**: `docker-compose -f compose/redis.yml up`
  Runs Catapulte with a Redis instance as the attachment backend.
- **Observability (OpenTelemetry)**: `docker-compose -f compose/observability.yml up`
  Exports traces and gauge metrics over OTLP to an [OpenTelemetry Collector](https://opentelemetry.io/docs/collector/); the collector derives RED metrics from spans (spanmetrics) and exposes everything to Prometheus at `http://localhost:9090`.


### Verifying the Setup

You can run an automated smoke test against all compose configurations by running:

```bash
just test-compose
```

This script will bring up each configuration, submit a test email, verify it reached Mailpit, and then shut down the services.

The container image and all compose files define a healthcheck that runs `catapulte healthcheck`. The subcommand probes `/health/ready` over HTTP and exits non-zero when a downstream dependency is unavailable. Operators deploying to Kubernetes can use an exec probe `["catapulte", "healthcheck"]` or a standard `httpGet` probe on `/health/ready`.

**Readiness scope.** `/health/ready` probes the **storage** backend and the **queue** backend (a live connection check when the queue is NATS; storage-backed and in-memory queues are covered by the storage probe). It deliberately does **not** probe the SMTP senders or the attachment store: SMTP outages are absorbed by the retry pipeline rather than making intake unready, and attachment-store outages only affect attachment-bearing submissions, not all traffic. `/health/live` is a process-liveness check that always returns `200`. The NATS probe verifies the client connection is up; it does not re-validate that the JetStream stream/consumer still exists, so a stream deleted after startup is not currently reflected in readiness.

## Usage

See the [usage guide](./docs/usage.md) for the full HTTP and NATS API: submitting
emails (plain/HTML, inline/named/remote MJML, attachments, batches), idempotency
and correlation ids, listing emails and lifecycle events, subscribing to events
over webhook or NATS, and the request/response shapes and limits.

A minimal submission:

```bash
curl -X POST http://localhost:3000/emails \
  -H "Content-Type: application/json" \
  -d '{
    "sender": "noreply@example.com",
    "recipients": [{ "kind": "to", "address": "alice@example.com" }],
    "subject": "Welcome",
    "body": { "kind": "plain", "text": "Hello Alice" }
  }'
# => {"id":"018f4e3c-2d1a-7b3c-8f00-1234567890ab"}
```

## Configuration

All configuration is done via environment variables.

### General

| Variable | Description | Default |
|----------|-------------|---------|
| `CATAPULTE_GC_SWEEP_INTERVAL_SECS` | Interval in seconds between garbage collection sweeps | `3600` |
| `CATAPULTE_GC_GRACE_PERIOD_SECS` | Minimum age for data to be eligible for garbage collection | `3600` |
| `CATAPULTE_BOUNCE_DOMAIN` | Domain of the signed return path (`bounces+{id}-{tag}@domain`) used as the SMTP `MAIL FROM`, so bounces can be tied to an email. Set together with `CATAPULTE_BOUNCE_SECRET`; setting only one stops startup. See [Bounce return path](#bounce-return-path) | unset (the sender address is used) |
| `CATAPULTE_BOUNCE_INGEST_SECRET` | Enables `POST /inbound/feedback` and is its bearer secret, at least 32 bytes (`openssl rand -base64 48`). Unset leaves the route out (404). API keys never work on that route. See [Bounce and complaint ingestion](#bounce-and-complaint-ingestion) | unset |
| `CATAPULTE_BOUNCE_SECRET` | Key that signs the return paths, at least 32 bytes; generate one with `openssl rand -base64 48`. Keep it stable: rotating it makes bounces for mail already sent unverifiable | unset |
| `CATAPULTE_ALLOWED_SENDER_DOMAINS` | Comma-separated sender domains the API accepts. Matching is exact (no subdomains), case-insensitive, and IDN domains are compared in punycode. Unset accepts any domain; set but empty denies every sender. An entry that is not a valid domain stops startup | unset |

### Storage Backend

| Variable | Description | Default |
|----------|-------------|---------|
| `CATAPULTE_STORAGE_BACKEND` | Storage engine: `sqlite` or `postgres` | `sqlite` |
| `CATAPULTE_SQLITE_URL` | SQLite connection string (e.g. `sqlite://catapulte.db`) | - |
| `CATAPULTE_POSTGRES_URL` | Postgres connection string (e.g. `postgres://user:pass@host/db`) | - |
| `CATAPULTE_POSTGRES_MAX_CONNECTIONS` | Maximum size of the Postgres connection pool | `10` |
| `CATAPULTE_POSTGRES_ACQUIRE_TIMEOUT_SECS` | Seconds to wait for a free pooled connection before erroring | `30` |

### Inbound Transports

#### HTTP
| Variable | Description | Default |
|----------|-------------|---------|
| `CATAPULTE_HTTP_ADDRESS` | Bind address for the HTTP server | - |
| `CATAPULTE_HTTP_API_KEY` | Legacy admin bearer token; with `CATAPULTE_API_KEYS` unset, unset here = no auth | - |
| `CATAPULTE_API_KEYS` | Comma-separated names of scoped API keys. Names use lowercase letters, digits and `_` | - |
| `CATAPULTE_API_KEY_{NAME}_SECRET` | **(Required per key)** The bearer token for this key. Secrets must be unique across all keys, including the legacy one | - |
| `CATAPULTE_API_KEY_{NAME}_SENDER_DOMAINS` | Comma-separated sender domains this key may use, intersected with `CATAPULTE_ALLOWED_SENDER_DOMAINS`. Set but empty denies every sender | unset (no extra limit) |
| `CATAPULTE_API_KEY_{NAME}_ADMIN` | `true` lets the key read every owner's emails and events | `false` |
| `CATAPULTE_HTTP_REQUEST_TIMEOUT_SECS` | Request deadline for read/list and health endpoints; the email submit routes are exempt so large attachment uploads over slow links are not truncated | 30 |

**Authentication:** set `CATAPULTE_HTTP_API_KEY` to a secret value and include `Authorization: Bearer <key>` on every request. The health endpoints (`/health/live`, `/health/ready`) are always public regardless of this setting. When neither `CATAPULTE_HTTP_API_KEY` nor any named key is set, the API is unauthenticated and every caller is a trusted administrator (startup logs a warning) — suitable only when running behind a trusted network boundary.

**Scoped API keys:** each named key is its own owner. Idempotency keys are unique per owner, so two keys can reuse the same `idempotency_key` without seeing each other's emails. A key that is not an admin only lists its own emails and events through `GET /emails`, `GET /events` and `GET /emails/{id}/events`; another owner's id returns an empty list. `POST /emails`, `POST /emails/batch` and `GET /senders` accept any key. The legacy `CATAPULTE_HTTP_API_KEY`, trusted mode and inbound NATS all share the default owner `""`, and the legacy key is an admin. A request body cannot set the owner or admin status. `GET /senders` is available to every key and exposes sender names, routing domains, quotas and aggregate sent and failed counts across all owners.

#### NATS

Inbound NATS is enabled by setting `CATAPULTE_INBOUND_NATS_URL`. When set, `_STREAM`, `_SUBJECT`, and `_CONSUMER` are required.

| Variable | Description | Default |
|----------|-------------|---------|
| `CATAPULTE_INBOUND_NATS_URL` | NATS server URL (on/off switch, leave unset to disable) | - |
| `CATAPULTE_INBOUND_NATS_STREAM` | **(Required)** JetStream stream name | - |
| `CATAPULTE_INBOUND_NATS_SUBJECT` | **(Required)** Subject for fire-and-forget submissions | - |
| `CATAPULTE_INBOUND_NATS_CONSUMER` | **(Required)** Pull consumer name | - |
| `CATAPULTE_INBOUND_NATS_ACK_WAIT_SECS` | Redelivery timeout | `30` |
| `CATAPULTE_INBOUND_NATS_MAX_DELIVER` | Maximum delivery attempts | `5` |
| `CATAPULTE_INBOUND_NATS_BACKOFF_SECS` | Comma-separated retry backoff steps in seconds | `1,5,30` |

### Outbound SMTP (Senders)

Multiple SMTP servers can be configured for routing.

- `CATAPULTE_SENDERS`: Comma-separated list of sender names (e.g. `primary,secondary`).

For each `{NAME}` in the list:

| Variable | Description | Default |
|----------|-------------|---------|
| `CATAPULTE_SENDER_{NAME}_KIND` | `smtp` or `sink` (see [Sink sender](#sink-sender-testing)) | `smtp` |
| `CATAPULTE_SENDER_{NAME}_HOST` | **(Required for `smtp`)** SMTP hostname | - |
| `CATAPULTE_SENDER_{NAME}_PORT` | SMTP port | `587` |
| `CATAPULTE_SENDER_{NAME}_USERNAME` | SMTP username | - |
| `CATAPULTE_SENDER_{NAME}_PASSWORD` | SMTP password | - |
| `CATAPULTE_SENDER_{NAME}_TLS` | `starttls`, `tls`, or `none` | `starttls` |
| `CATAPULTE_SENDER_{NAME}_PRIORITY` | Lower numbers are tried first | `100` |
| `CATAPULTE_SENDER_{NAME}_QUOTA_COUNT` | Max emails allowed in range | - |
| `CATAPULTE_SENDER_{NAME}_QUOTA_RANGE` | `hourly`, `daily`, `weekly`, or `monthly` | - |
| `CATAPULTE_SENDER_{NAME}_MATCH_DOMAIN` | Optional domain to strictly route traffic for | - |

**Connection pooling:** each configured sender reuses its SMTP connections instead of dialing
the server for every message, so the per-send connection setup cost is paid once and then
amortised. Expect each sender to keep up to one idle connection open per process for about a
minute between sends. Pool size and idle timeout are not configurable yet (there is no concurrent
in-flight sending today that would make a larger pool useful); the knobs will arrive with
intra-worker concurrency. If a pooled connection was dropped by the server while idle, the next
send on it fails and is retried through the normal queue retry and alternate-sender fallback.

#### Sink sender (testing)

A sender with `CATAPULTE_SENDER_{NAME}_KIND=sink` needs no host, port, credentials or TLS. Emails
go through validation, rendering, attachment handling and events as usual and reach
`delivery.succeeded`, but nothing is sent over SMTP: the message is built and then discarded.
Use it in CI and staging. Priority, quota and `MATCH_DOMAIN` routing apply to a sink like any
other sender, and startup logs a warning for each sink. Do not mix a sink into production
routes: when an SMTP sender fails or is over quota, an eligible sink accepts the message,
discards it and reports `delivery.succeeded`. Submission still checks the estimated size against
`CATAPULTE_MAX_MESSAGE_BYTES`; only the final wire-size check is skipped for a sink.

#### Bounce return path

With `CATAPULTE_BOUNCE_DOMAIN` and `CATAPULTE_BOUNCE_SECRET` set, each message is sent with
`MAIL FROM:<bounces+{email id}-{tag}@{bounce domain}>`, where the tag is a keyed hash of the
email id. The `From` header and the recipients' view of the message do not change. The SMTP
recipient list is built from the final To, Cc and Bcc recipients (minus suppressed ones), never
from the message headers, so a Bcc recipient still receives the message without a `Bcc` header.

DNS needs two things:

- **SPF** for the bounce domain must authorise the relay you send through, because the
  receiving server checks SPF against the `MAIL FROM` domain.
- **DMARC** alignment does not come from the bounce domain. It comes from DKIM signed for the
  `From` domain (see below), so keep DKIM configured for every sender domain.

The bounce domain also needs an MX that receives the bounces.

#### Bounce and complaint ingestion

Point the MX of your bounce domain at a command that forwards each received message, unchanged,
to Catapulte. With Postfix, a `pipe` transport such as

```
# master.cf
catapulte-feedback unix - n n - - pipe
  flags=Rq user=nobody argv=/usr/bin/curl -sf --max-time 20 --data-binary @- \
    -H "Authorization: Bearer $CATAPULTE_BOUNCE_INGEST_SECRET" \
    http://catapulte:3000/inbound/feedback
```

routes `bounces+*@your-bounce-domain` to it (`transport_maps`). Forward only mail addressed to the
bounce domain.

Catapulte accepts delivery status notifications (RFC 3464) and abuse reports (RFC 5965) and
answers `204`. Anything else, an unknown email, or a recipient the email was not sent to
returns `422` and is not stored. The ingest secret only authenticates the forwarder: the
report is untrusted, and is tied to an email through the signed return path (see above).

- A hard bounce (`Action: failed`, status `5.x.x`) emits a `bounced` event; a soft bounce
  emits the same event with `hard: false`. A complaint emits `complained`.
- **Automatic suppression** happens only for a hard bounce, or a complaint, whose report
  carries a valid signed return path, **and only when the email had exactly one recipient**
  (To, Cc and Bcc together). The signed tag identifies the email, not a recipient, so on a
  multi-recipient email any recipient could forge a report about another one; those emails
  get the event only.
- A complaint without the return path, matched by the original `Message-ID` alone, emits
  `complained` with `verified: false` and never suppresses. A tag that is present but invalid
  is rejected outright.

#### DKIM signing

Catapulte signs a message when a key is configured for the domain of its sender address.
Set `CATAPULTE_DKIM_DOMAINS` to a comma-separated list of names, then for each `NAME`:

| Variable | Description | Default |
|----------|-------------|---------|
| `CATAPULTE_DKIM_{NAME}_DOMAIN` | **(Required)** signing domain (`d=`) | - |
| `CATAPULTE_DKIM_{NAME}_SELECTOR` | **(Required)** selector (`s=`) | - |
| `CATAPULTE_DKIM_{NAME}_PRIVATE_KEY_FILE` | **(Required)** path to the key file, read and trimmed at startup | - |
| `CATAPULTE_DKIM_{NAME}_ALGORITHM` | `rsa` or `ed25519` | `rsa` |

```bash
CATAPULTE_DKIM_DOMAINS=main
CATAPULTE_DKIM_MAIN_DOMAIN=example.com
CATAPULTE_DKIM_MAIN_SELECTOR=s2026
CATAPULTE_DKIM_MAIN_PRIVATE_KEY_FILE=/run/secrets/dkim.key
CATAPULTE_DKIM_MAIN_ALGORITHM=ed25519
```

A sender uses the entry whose domain equals its domain or is its longest parent domain
(`news.example.com` uses `example.com`; `badexample.com` does not). Matching ignores case. A
key that cannot be loaded stops startup. Signing uses relaxed/relaxed canonicalization over
`From`, `Subject`, `To`, `Date`, `Message-ID`, `Reply-To`, `List-Unsubscribe` and
`List-Unsubscribe-Post` (those present), and happens before the size limit check.
Domains are lowercased and converted to punycode, in the configuration and for each sender. A
parent-domain match (`d=example.com` for `news.example.com`) satisfies only relaxed DMARC
alignment; `adkim=s` needs an exact match. Startup logs the configured DKIM domains, never keys.

Key formats:

- **Ed25519 (recommended):** a file holding the base64 of the raw 32-byte seed, not PEM.
  `openssl rand -base64 32 > dkim.key` creates one; the public key for DNS is the base64 of the
  matching 32-byte public key.
- **RSA:** a PKCS#1 PEM (`-----BEGIN RSA PRIVATE KEY-----`). A PKCS#8 key
  (`-----BEGIN PRIVATE KEY-----`) is rejected; convert it with
  `openssl rsa -traditional -in key.pem -out key-pkcs1.pem`.

Publish the public key as a TXT record at `<selector>._domainkey.<domain>`:

```
s2026._domainkey.example.com. TXT "v=DKIM1; k=ed25519; p=<base64 public key>"
s2026._domainkey.example.com. TXT "v=DKIM1; k=rsa; p=<base64 DER public key>"
```

Prefer Ed25519: the `rsa` crate used for RSA signing carries RUSTSEC-2023-0071 (a timing
side channel in private-key operations). Some receivers do not yet verify Ed25519 signatures.

### Email Queue

| Variable | Description | Default |
|----------|-------------|---------|
| `CATAPULTE_QUEUE_BACKEND` | `storage`, `memory`, or `nats` | `storage` |

#### NATS Queue (if backend is `nats`)
Enqueue waits for the JetStream publish acknowledgement before it succeeds.

| Variable | Description | Default |
|----------|-------------|---------|
| `CATAPULTE_QUEUE_URL` | **(Required)** NATS server URL | - |
| `CATAPULTE_QUEUE_STREAM` | JetStream stream name | `CATAPULTE_EMAILS` |
| `CATAPULTE_QUEUE_SUBJECT` | JetStream subject | `catapulte.emails.queued` |
| `CATAPULTE_QUEUE_CONSUMER` | Pull consumer name | `catapulte-worker` |
| `CATAPULTE_QUEUE_ACK_WAIT_SECS` | Redelivery timeout | `30` |
| `CATAPULTE_QUEUE_MAX_DELIVER` | Maximum delivery attempts | `3` |
| `CATAPULTE_QUEUE_BACKOFF` | Comma-separated retry backoff steps in seconds | `30,60,120` |

##### Delivery model and recovery

Delivery is at least once. Each publish carries a `Nats-Msg-Id` header equal to the email id, so JetStream drops a republish of the same email inside the stream's duplicate window (2 minutes by default). Outside that window a recovery can deliver the message twice. Nothing here provides exactly-once delivery.

When the publish ack is lost or times out, Catapulte cannot tell whether the stream stored the message. It keeps the row (`enqueue_state = 'publishable'`) and the attachment blobs, and the submit fails with `500`. A retry with the same idempotency key publishes the stored envelope again. Without an idempotency key there is nothing to match, so the retry creates a new email. Use idempotency keys for NATS submissions.

A submission that never finished (still in progress, or its cleanup failed) stays in `enqueue_state = 'reserved'`. A retry with its idempotency key gets `409` until an operator removes the row. There is no automatic expiry for `publishable` or `reserved` rows.

To list rows that are old and show no worker activity, run the query below (Postgres: replace the time test with `e.created_at < (EXTRACT(EPOCH FROM NOW()) - 3600) * 1000`):

```sql
SELECT e.id, e.idempotency_key, e.enqueue_state
FROM emails e
WHERE e.enqueue_state IN ('publishable', 'reserved')
  AND e.created_at_ms < (unixepoch() - 3600) * 1000
  AND NOT EXISTS (
    SELECT 1 FROM lifecycle_events le
    WHERE le.email_id = e.id
      AND le.event_type IN ('sending', 'retrying', 'delivery.succeeded', 'delivery.failed')
  );
```

Clean up a `publishable` or `reserved` row only in this order:

1. Quiesce. Stop the Catapulte instances, or at least stop submit traffic, and let the workers drain.
2. Run `nats stream info <stream>`. Delete only when it reports `Messages: 0`. The stream uses work-queue retention, so acknowledged messages are removed and an empty stream cannot hold a message for the row. If the stream is not empty, do not delete: wait for it to drain, or leave the rows. Listing or scanning messages is not a safe substitute, because it cannot prove that a message for the row is absent.
3. Only then delete the row. The attachment garbage collector removes the blobs of deleted rows.

Apply the same rule to a `reserved` row: quiesce, delete only when the stream reports `Messages: 0`, and otherwise leave it.

### Worker

| Variable | Description | Default |
|----------|-------------|---------|
| `CATAPULTE_WORKER_CONCURRENCY` | Maximum number of emails the worker processes concurrently | `1` |
| `CATAPULTE_MAX_MESSAGE_BYTES` | Maximum size of an email on the wire (also used for the early estimate at submit); must be a positive integer | `26214400` (25 MiB) |

**Memory.** A worker holds the resolved attachment bytes, the copy inside the MIME part, the encoded message and its formatted wire bytes, so an email near the limit needs roughly `3.5 × CATAPULTE_MAX_MESSAGE_BYTES` of memory in the worst case, and the worker needs about `CATAPULTE_WORKER_CONCURRENCY × 3.5 × CATAPULTE_MAX_MESSAGE_BYTES` (about 88 MiB per concurrent email with the default). This is an approximation, not a measured bound. Set the limit at or below your relay's own limit, minus any headers the relay adds.

**Choosing a safe concurrency value.** Every in-flight send touches both the DB (event publish, ack/nack, `set_attachments`) and an SMTP connection, so the practical ceiling is `min(SMTP pool size = 10, CATAPULTE_POSTGRES_MAX_CONNECTIONS)` with headroom left for the HTTP submit path and background GC. A formula like `concurrency = pool_size - 2` is a reasonable starting point.

**SQLite caveat.** SQLite uses a single connection (`max_connections(1)`), so raising concurrency above `1` just serializes all DB calls on that one connection and risks acquire timeouts rather than improving throughput. Only raise `CATAPULTE_WORKER_CONCURRENCY` when using Postgres, and raise `CATAPULTE_POSTGRES_MAX_CONNECTIONS` to match.

**Quota note.** Sender quotas are counted from committed `Sent` events and are checked before sending. With concurrency > 1 the read-to-send window widens, so a quota may be overshot by up to ~concurrency before the next event is committed. This is accepted: quotas are best-effort and eventually consistent by design.

### Event Publishers (Observability)

| Variable | Description | Default |
|----------|-------------|---------|
| `CATAPULTE_WEBHOOK_URL` | URL to POST lifecycle events to | - |
| `CATAPULTE_WEBHOOK_TIMEOUT_MS` | Webhook call timeout | `5000` |
| `CATAPULTE_WEBHOOK_SECRET` | **(Optional)** Shared secret used to sign webhook requests with HMAC-SHA256; see [Verifying webhook signatures](docs/usage.md#verifying-webhook-signatures); treated as secret, never logged | - |
| `CATAPULTE_NATS_EVENTS_URL` | NATS server for event publishing | - |
| `CATAPULTE_NATS_EVENTS_SUBJECT` | Subject for lifecycle events | `catapulte.lifecycle` |

### Template Management

#### Template Resolver
| Variable | Description | Default |
|----------|-------------|---------|
| `CATAPULTE_RESOLVER_ALLOWED_DOMAINS` | Allowed domains for remote MJML fetching | - |
| `CATAPULTE_RESOLVER_TEMPLATES_DIR` | Directory containing `.mjml` templates | - |
| `CATAPULTE_RESOLVER_FETCH_TIMEOUT_MS` | Total timeout for one remote template fetch, in milliseconds; redirects are never followed | `30000` |
| `CATAPULTE_RESOLVER_MAX_BYTES` | Maximum size in bytes of a remote template response | `1048576` |
| `CATAPULTE_RESOLVER_TOKENS` | Comma-separated names of auth entries (e.g. `github,gitlab`); absent or empty means no auth | - |
| `CATAPULTE_RESOLVER_TOKEN_<NAME>_HOST` | Exact host the named entry's token is attached to (must also be in `ALLOWED_DOMAINS`) | - |
| `CATAPULTE_RESOLVER_TOKEN_<NAME>_BEARER_TOKEN` | **(Optional)** Sent as `Authorization: Bearer <token>` only to the matching host; treated as secret, never logged | - |
| `CATAPULTE_RESOLVER_TOKEN_<NAME>_HEADERS` | Comma-separated list of header names to attach to requests to the matching host (e.g. `Accept,PRIVATE-TOKEN`) | - |
| `CATAPULTE_RESOLVER_TOKEN_<NAME>_HEADER_<FRAGMENT>_VALUE` | Value for the named header; FRAGMENT is the header name uppercased with `-` replaced by `_` (e.g. `ACCEPT`, `PRIVATE_TOKEN`); treated as secret, never logged | - |

Besides the files in `CATAPULTE_RESOLVER_TEMPLATES_DIR`, an admin can upload named templates over the API
(see [Templates](docs/usage.md#templates)). The files are authoritative: a name that exists on disk
is always rendered from disk and cannot be changed through the API.

Each auth entry must configure at least a bearer token or one header. Defining a host with neither is a configuration error.

**Examples**

Private GitHub raw content via bearer token plus a custom `Accept` header:

```sh
CATAPULTE_RESOLVER_TOKENS=github
CATAPULTE_RESOLVER_TOKEN_GITHUB_HOST=api.github.com
CATAPULTE_RESOLVER_TOKEN_GITHUB_BEARER_TOKEN=ghp_xxxx
CATAPULTE_RESOLVER_TOKEN_GITHUB_HEADERS=Accept
CATAPULTE_RESOLVER_TOKEN_GITHUB_HEADER_ACCEPT_VALUE=application/vnd.github.raw
```

GitLab via a `PRIVATE-TOKEN` header and no bearer:

```sh
CATAPULTE_RESOLVER_TOKENS=gitlab
CATAPULTE_RESOLVER_TOKEN_GITLAB_HOST=gitlab.com
CATAPULTE_RESOLVER_TOKEN_GITLAB_HEADERS=PRIVATE-TOKEN
CATAPULTE_RESOLVER_TOKEN_GITLAB_HEADER_PRIVATE_TOKEN_VALUE=glpat-xxxx
```

#### MJML Include Loader
| Variable | Description | Default |
|----------|-------------|---------|
| `CATAPULTE_INCLUDE_LOADER_FS_ROOT` | Local root for `<mj-include>` | - |
| `CATAPULTE_INCLUDE_LOADER_HTTP_ALLOW` | Exact origins allowed for HTTP includes (e.g. `https://example.com`); unset disables HTTP includes | - |
| `CATAPULTE_INCLUDE_LOADER_HTTP_TIMEOUT_MS` | Total timeout for one HTTP include fetch, in milliseconds; redirects are never followed | `30000` |
| `CATAPULTE_INCLUDE_LOADER_HTTP_MAX_BYTES` | Maximum size in bytes of an HTTP include response | `1048576` |

#### Render sandbox
Every template render (send, validation and preview) runs in a short-lived child process of the same binary, started as `catapulte __render-sandbox`, with CPU, memory and wall-clock limits. The child has a 2 s CPU limit (killed at 3 s), and an empty environment apart from `MALLOC_ARENA_MAX=2`, the `SSL_CERT_*` and proxy variables, and `REQUEST_METHOD`. Run the container as a user that can execute the binary.

| Variable | Description | Default |
|----------|-------------|---------|
| `CATAPULTE_RENDER_TIMEOUT_MS` | Deadline for one render stage, covering the wait for a free slot, the child's work and its cleanup. A render that hits it fails permanently | `7000` |
| `CATAPULTE_RENDER_MAX_MEMORY_BYTES` | Address-space limit of each child, in bytes | `536870912` |
| `CATAPULTE_RENDER_CONCURRENCY` | Children running at the same time, shared by sends, validation and preview; further renders wait | available CPU cores |

A send renders in up to two stages (interpolate, then MJML) and an upload or preview in two, so startup refuses to run unless `2 x (CATAPULTE_RENDER_TIMEOUT_MS + 1 s) + 5 s` is at most the smaller of `CATAPULTE_HTTP_REQUEST_TIMEOUT_SECS` and the 30 s shutdown drain. With the default that is 21 s. This bounds the time spent rendering, not the whole request, which also includes storage and SMTP. The HTTP include timeout is lowered to half of `CATAPULTE_RENDER_TIMEOUT_MS` when it is longer, and a warning is logged.

Memory use in the worst case is the process itself plus, for every slot, `CATAPULTE_RENDER_MAX_MEMORY_BYTES` and about 36 MiB of request and response buffers. Size the container for `CATAPULTE_RENDER_CONCURRENCY` times that. Renders waiting for a slot hold only their own inputs; the request is built once a slot is free. A child that cannot be reaped (for example one stuck in uninterruptible I/O) keeps its slot until it exits. The memory limit is on address space, so it is a ceiling and not the resident size.

### Attachments

#### Attachment Store

Catapulte supports storing attachments on the local filesystem (`fs`, default), in any S3-compatible object store such as MinIO or Cloudflare R2 (`s3`), or in Redis (`redis`). The garbage collector sweeps all backends, removing orphaned objects older than the configured grace period.

| Variable | Description | Default |
|----------|-------------|---------|
| `CATAPULTE_ATTACHMENT_BACKEND` | Attachment backend: `fs`, `s3`, or `redis` | `fs` |
| `CATAPULTE_ATTACHMENT_FS_ROOT` | Directory for attachment storage (when backend is `fs`) | - |
| `CATAPULTE_ATTACHMENT_S3_ENDPOINT` | **(Required)** S3-compatible endpoint URL (e.g. `http://localhost:9000` for MinIO) | - |
| `CATAPULTE_ATTACHMENT_S3_REGION` | AWS region or region hint for the endpoint | `us-east-1` |
| `CATAPULTE_ATTACHMENT_S3_BUCKET` | **(Required)** Bucket name | - |
| `CATAPULTE_ATTACHMENT_S3_ACCESS_KEY_ID` | **(Required)** Access key ID | - |
| `CATAPULTE_ATTACHMENT_S3_SECRET_ACCESS_KEY` | **(Required)** Secret access key | - |
| `CATAPULTE_ATTACHMENT_S3_PATH_STYLE` | Use path-style addressing (keep `true` for MinIO and most self-hosted gateways) | `true` |
| `CATAPULTE_ATTACHMENT_S3_PREFIX` | Object key prefix / folder within the bucket | - |
| `CATAPULTE_ATTACHMENT_REDIS_URL` | **(Required)** Redis connection URL (e.g. `redis://localhost:6379`, or `rediss://` for TLS) | - |
| `CATAPULTE_ATTACHMENT_REDIS_PREFIX` | Key prefix / namespace for stored blobs | - |

#### Attachment Fetcher
| Variable | Description | Default |
|----------|-------------|---------|
| `CATAPULTE_ATTACHMENT_FETCHER_ALLOWED_DOMAINS` | Allowed domains for fetching | - |
| `CATAPULTE_ATTACHMENT_FETCHER_ALLOW_HTTP` | Allow non-HTTPS fetches | `false` |
| `CATAPULTE_ATTACHMENT_FETCHER_MAX_BYTES` | Max size per attachment | `25MiB` |
| `CATAPULTE_ATTACHMENT_FETCHER_FETCH_TIMEOUT_MS` | Fetch timeout | `30000` |

### Observability (OTLP Tracing and Metrics)

All variables accept a `CATAPULTE_OTEL_` prefix that takes precedence over the standard `OTEL_*` equivalents when both are set.

A ready-to-run example wiring Catapulte to an OpenTelemetry Collector (with the spanmetrics connector) and Prometheus lives at [`compose/observability.yml`](./compose/observability.yml); see [`compose/otel-collector.yaml`](./compose/otel-collector.yaml) for the collector pipeline.

#### Traces

| Variable | `OTEL_*` fallback | Description | Default |
|----------|-------------------|-------------|---------|
| `CATAPULTE_OTEL_TRACES_EXPORTER` | `OTEL_TRACES_EXPORTER` | Traces backend: `otlp` to enable export, `none` to disable | `none` |
| `CATAPULTE_OTEL_EXPORTER_OTLP_PROTOCOL` | `OTEL_EXPORTER_OTLP_PROTOCOL` | Wire protocol: `grpc` or `http/protobuf` | `grpc` |
| `CATAPULTE_OTEL_EXPORTER_OTLP_ENDPOINT` | `OTEL_EXPORTER_OTLP_ENDPOINT` | **(Required when traces enabled)** Collector endpoint URL (e.g. `http://collector:4317`) | - |
| `CATAPULTE_OTEL_EXPORTER_OTLP_HEADERS` | `OTEL_EXPORTER_OTLP_HEADERS` | Additional headers sent with each export request, `k=v,k=v` format | - |
| `CATAPULTE_OTEL_SERVICE_NAME` | `OTEL_SERVICE_NAME` | `service.name` resource attribute | `catapulte` |
| `CATAPULTE_OTEL_SERVICE_INSTANCE_ID` | `OTEL_SERVICE_INSTANCE_ID` | `service.instance.id` resource attribute — distinguishes replicas in metrics/traces | `$HOSTNAME`, else a random UUID |

The `service.version` resource attribute is always set to the binary's compiled-in crate version. `service.instance.id` is what keeps each replica's traces and gauge time-series distinct when you run more than one instance.

#### Metrics

Catapulte emits gauges over OTLP. RED metrics (request rate, error rate, duration) are collector-derived from traces; the application only pushes application-level gauges directly.

| Variable | Description | Default |
|----------|-------------|---------|
| `CATAPULTE_OTEL_METRICS_EXPORTER` | Metrics backend: `otlp` to enable export, `none` to disable | `none` |
| `CATAPULTE_OTEL_METRIC_EXPORT_INTERVAL_SECS` | How often the sampler pushes gauges to the collector (seconds). The standard `OTEL_METRIC_EXPORT_INTERVAL` is intentionally not aliased — it uses milliseconds which creates a unit mismatch | `60` |

When metrics are enabled the endpoint, protocol, and headers are reused from the traces configuration (`CATAPULTE_OTEL_EXPORTER_OTLP_*`). No separate metrics endpoint variable is needed.

Emitted gauges:

| Metric | Labels | Description |
|--------|--------|-------------|
| `catapulte.queue.pending` | `backend` (sqlite, postgres, memory, nats) | Number of email queue entries eligible to be claimed |
| `catapulte.sender.sent_in_range` | `sender` | Emails sent by this sender within its quota window |
| `catapulte.sender.quota_limit` | `sender` | Configured quota count for the sender (omitted when no quota is set) |

## Out of scope (for now)

Scheduled sends. Listed so they aren't mistaken for missing stories.

## License

Catapulte is licensed under the GNU Affero General Public License v3.0. See [LICENSE](./LICENSE).
