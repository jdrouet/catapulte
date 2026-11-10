# Using Catapulte

This guide shows how to **use** a running Catapulte instance: submitting emails,
reading their state, and subscribing to delivery events. For how to **run and
configure** a server (storage, SMTP senders, queue, observability), see the
[readme](../readme.md).

Catapulte accepts an email, returns a tracking id immediately, and owns SMTP
delivery, routing, retries, and lifecycle events from there.

- Base URL in the examples below: `http://localhost:3000`.
- All request/response bodies are JSON unless noted (attachment uploads may use
  `multipart/form-data`).

## Authentication

If the server sets `CATAPULTE_HTTP_API_KEY`, every endpoint **except** the health
probes requires a bearer token:

```bash
curl http://localhost:3000/emails \
  -H "Authorization: Bearer $CATAPULTE_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{ ... }'
```

A missing or wrong token returns `401`. When the key is unset, the API is
unauthenticated (only safe behind a trusted network boundary).

## Submitting an email

`POST /emails` accepts one email and returns its tracking id:

```json
{ "id": "018f4e3c-2d1a-7b3c-8f00-1234567890ab" }
```

The id is a UUIDv7. The email is queued; delivery happens asynchronously (watch
[lifecycle events](#lifecycle-events) to observe the outcome).

### Required fields

| Field | Type | Notes |
|-------|------|-------|
| `sender` | string or object | `"noreply@example.com"`, `"Acme <noreply@example.com>"` or `{ "name": "Acme", "address": "noreply@example.com" }` |
| `recipients` | array | non-empty; each `{ "kind": "to" \| "cc" \| "bcc", "address": "<email>", "name": "<optional display name>" }`; `address` may be `Name <email>` only when `name` is not set |
| `body` | object | a [body variant](#body-variants) (tagged by `kind`) |

Addresses accept a display name, which is kept in the `From:` and recipient headers
and RFC 2047-encoded when it is not ASCII. Use the structured form when a name
contains commas or quotes. SMTP routing, sender-domain matching and the `recipient`
filter use only the bare address. `GET /emails` returns `sender` and recipient
addresses in canonical form (`Name <email>` when a name was given). A value that cannot be parsed as a
mailbox is rejected with `400`, as are display names with control characters and
addresses with an IP-literal domain or a quoted local part, which the SMTP library cannot put in message headers.

### Optional fields

| Field | Type | Notes |
|-------|------|-------|
| `subject` | string | |
| `idempotency_key` | string | retry-safe submission (see [Idempotency](#idempotency)) |
| `correlation_id` | string | echoed back on lifecycle events; use it to correlate without a synchronous id |
| `variables` | object | template variables; defaults to `{}` |
| `attachments` | array | see [Attachments](#attachments); defaults to `[]` |

### Body variants

`body` is a tagged union on `kind`:

| `kind` | Fields | Description |
|--------|--------|-------------|
| `plain` | `text` and/or `html` (at least one) | a ready-made plain-text and/or HTML body |
| `mjml_inline` | `source` | raw MJML source, rendered with `variables` |
| `mjml_named` | `name` | a template pre-registered on the server, rendered with `variables` |
| `mjml_remote` | `url` | MJML fetched over HTTP (supports `mj-include`), rendered with `variables` |

### Examples

Plain text + HTML:

```bash
curl -X POST http://localhost:3000/emails \
  -H "Content-Type: application/json" \
  -d '{
    "sender": "noreply@example.com",
    "recipients": [{ "kind": "to", "address": "alice@example.com" }],
    "subject": "Welcome",
    "body": { "kind": "plain", "text": "Hello Alice", "html": "<p>Hello Alice</p>" }
  }'
```

Inline MJML with variables:

```bash
curl -X POST http://localhost:3000/emails \
  -H "Content-Type: application/json" \
  -d '{
    "sender": "noreply@example.com",
    "recipients": [{ "kind": "to", "address": "alice@example.com" }],
    "subject": "Hi {{ name }}",
    "body": { "kind": "mjml_inline", "source": "<mjml><mj-body><mj-text>Hi {{ name }}</mj-text></mj-body></mjml>" },
    "variables": { "name": "Alice" }
  }'
```

A pre-registered template (`mjml_named`) or a remote one (`mjml_remote` with a
`url`) follow the same shape, swapping the `body` object.

### Attachments

Up to **10** attachments per email. Each is either inline base64 **or** a remote
URL (exactly one):

```json
"attachments": [
  { "filename": "invoice.pdf", "content_type": "application/pdf", "inline_base64": "<base64>" },
  { "filename": "logo.png",    "content_type": "image/png",       "url": "https://cdn.example.com/logo.png" }
]
```

Remote URLs are fetched server-side subject to the operator's allow-list; an
unreachable or disallowed URL fails the submission with `400`.

#### Streaming uploads (multipart)

To avoid base64 overhead for large files, submit `multipart/form-data` with one
`envelope` JSON part (the email **without** the `attachments` field) and one
`attachment` part per file:

```bash
curl -X POST http://localhost:3000/emails \
  -F 'envelope={"sender":"noreply@example.com","recipients":[{"kind":"to","address":"alice@example.com"}],"subject":"Report","body":{"kind":"plain","text":"see attached"}};type=application/json' \
  -F 'attachment=@./report.pdf;type=application/pdf'
```

Each `attachment` part's filename and content type come from its
`Content-Disposition`/`Content-Type`. The submit routes are exempt from the HTTP
request timeout so large uploads over slow links are not truncated.

### Idempotency

Pass an `idempotency_key` to make retries safe. If a submission reuses a key that
already exists, Catapulte returns the **existing** email's id (`200`) and does not
send a second copy.

## Submitting a batch

`POST /emails/batch` accepts up to **100** emails and reports per-email outcomes
(partial acceptance — valid emails are accepted even if others are rejected):

```bash
curl -X POST http://localhost:3000/emails/batch \
  -H "Content-Type: application/json" \
  -d '{
    "emails": [
      { "sender": "noreply@example.com", "recipients": [{ "kind": "to", "address": "a@example.com" }], "body": { "kind": "plain", "text": "hi" } },
      { "sender": "noreply@example.com", "recipients": [], "body": { "kind": "plain", "text": "hi" } }
    ]
  }'
```

```json
{
  "results": [
    { "status": "accepted", "id": "018f4e3c-2d1a-7b3c-8f00-1234567890ab" },
    { "status": "rejected", "error": "recipients must not be empty" }
  ]
}
```

`results` is positional (aligned to the input `emails`). A per-email *validation*
error is reported as `rejected`; an infrastructure failure aborts the whole batch
with `500`. Batch items use the inline/remote attachment form (no multipart).

## Listing emails

`GET /emails` returns your submitted emails, newest-first, paginated.

| Query param | Notes |
|-------------|-------|
| `status` | `queued` \| `sent` \| `failed` |
| `recipient` | substring filter on the recipient's bare email address; display names are not matched |
| `template` | filter by named MJML template name; only matches emails submitted with `kind: mjml_named` |
| `id` | exact email id (UUID) |
| `after_ms`, `before_ms` | created-at bounds, Unix epoch ms |
| `limit` | default 20, max 100 |
| `offset` | default 0 |

```bash
curl "http://localhost:3000/emails?status=failed&limit=50"
```

```json
{
  "emails": [
    {
      "id": "018f4e3c-2d1a-7b3c-8f00-1234567890ab",
      "idempotency_key": null,
      "subject": "Welcome",
      "sender": "noreply@example.com",
      "recipients": [{ "kind": "to", "address": "alice@example.com" }],
      "created_at_ms": 1700000000000,
      "status": "sent"
    }
  ],
  "limit": 20,
  "offset": 0
}
```

## Lifecycle events

Every email moves through a sequence of events. You can poll them or subscribe to
them in real time.

### Reading events

- `GET /events` — across all emails. Filters: `email_id`, `event_type`,
  `sender_name` (the upstream SMTP server), `error_class`, `after_ms`,
  `before_ms`, `limit`, `offset`.
- `GET /emails/{id}/events` — events for one email (same filters).

`error_class` is validated against the vocabulary below — an unknown value
returns `400`. `event_type` and `sender_name` are free-form.

```bash
curl "http://localhost:3000/emails/018f4e3c-2d1a-7b3c-8f00-1234567890ab/events"
```

```json
{
  "events": [
    {
      "id": "018f...",
      "email_id": "018f4e3c-2d1a-7b3c-8f00-1234567890ab",
      "event_type": "delivery.succeeded",
      "payload": { "sender_name": "primary", "correlation_id": "order-12345" },
      "sender_name": "primary",
      "error_class": null,
      "created_at_ms": 1700000000050
    }
  ],
  "limit": 20,
  "offset": 0
}
```

### Subscribing (webhook / NATS)

When the operator configures a webhook URL or a NATS subject, Catapulte pushes
each event as JSON:

```json
{
  "event_type": "delivery.succeeded",
  "email_id": "018f4e3c-2d1a-7b3c-8f00-aabbccddeeff",
  "payload": { "sender_name": "primary", "correlation_id": "order-12345" }
}
```

| `event_type` | Meaning | `payload` fields |
|--------------|---------|------------------|
| `queued` | accepted and enqueued | `correlation_id` |
| `sending` | a delivery attempt is starting | `attempt`, `correlation_id` |
| `delivery.succeeded` | accepted by the upstream SMTP server | `sender_name`, `correlation_id` |
| `retrying` | attempt failed, will retry | `attempt`, `reason`, `error_class`, `sender_name`, `correlation_id` |
| `delivery.failed` | retries exhausted | `attempt`, `reason`, `error_class`, `sender_name`, `correlation_id` |

`attempt` counts from 1; `sender_name`/`correlation_id` may be null. `error_class`
is present on `retrying` / `delivery.failed` only, and is one of `template_resolve`,
`template_interpolate`, `template_render`, `attachment`, `delivery`, `rejected`,
`routing`. `delivery` is a transient delivery failure, retried up to the attempt limit.
`rejected` is a terminal message-level rejection: a bad recipient or mailbox, content the
server refuses, or a message that cannot be built locally (which has no SMTP code). It is
not retried or sent to other routes, but it can follow earlier transient attempts.
Route-level failures (authentication, policy, size limits) fall back to other routes,
retry, and end as `delivery`.
(The pushed payload has no timestamp; the stored events from `GET /events` carry
`created_at_ms`.) Webhooks are retried a few times on a non-2xx response.

#### Verifying webhook signatures

When the operator sets `CATAPULTE_WEBHOOK_SECRET`, each webhook request carries
`X-Catapulte-Timestamp` (unix seconds) and `X-Catapulte-Signature`
(`sha256=` followed by the lowercase hex HMAC-SHA256 of `<timestamp>.<raw body>`,
keyed with the secret). The timestamp is refreshed on every retry. A receiver
should:

1. Reject the request if the timestamp differs from the current time by more
   than 300 seconds.
2. Recompute the HMAC over the timestamp, a `.` and the raw request body bytes,
   before any JSON parsing or re-serialisation.
3. Compare the result with the header in constant time.
4. Optionally keep the `(timestamp, signature)` pairs seen within the window and
   reject repeats to block replays.

```python
import hashlib, hmac, time

def verify(secret: bytes, timestamp: str | None, signature: str | None, raw_body: bytes) -> bool:
    try:
        if abs(time.time() - int(timestamp)) > 300:
            return False
        expected = "sha256=" + hmac.new(
            secret, timestamp.encode() + b"." + raw_body, hashlib.sha256
        ).hexdigest()
        return hmac.compare_digest(expected.encode(), signature.encode())
    except (TypeError, ValueError, AttributeError, OverflowError):
        return False
```

Without a secret, no signature headers are sent and anyone who knows the URL can
post forged events, so keep the URL private or set a secret.

## Submitting over NATS (fire-and-forget)

If the operator enables the NATS inbound transport, publish the **same JSON** as
`POST /emails` to the configured subject. NATS submission is fire-and-forget:
there is no synchronous tracking id. Supply a `correlation_id` in the payload and
observe the outcome via lifecycle events.

## Listing senders

`GET /senders` reports the configured upstream SMTP senders and their usage within
the current quota window:

```json
{
  "senders": [
    { "name": "primary", "sent_in_range": 42, "failed_in_range": 3, "quota": { "count": 1000, "range": "daily" } }
  ]
}
```

`quota` is null when none is configured; `range` is `hourly` | `daily` | `weekly`
| `monthly`.

## Health

Always public (never require the API key):

- `GET /health/live` → `200 {"status":"ok"}` — process liveness.
- `GET /health/ready` → `200 {"status":"ok"}` when storage and the queue are
  reachable, `503 {"status":"unavailable"}` otherwise.

## Errors

Errors return a minimal JSON body (details are logged server-side, not returned):

```json
{ "error": "invalid request" }
```

| Status | When |
|--------|------|
| `400` | malformed JSON/multipart, validation failure (sender/recipients/body/attachment), bad UUID, unreachable/disallowed remote attachment, batch over 100 |
| `401` | missing/invalid bearer token |
| `500` | storage / queue / attachment-store failure |

## Limits

| Limit | Value |
|-------|-------|
| Max request body | 352 MiB |
| Max envelope JSON (multipart `envelope` part) | 1 MiB |
| Max size per attachment | 25 MiB |
| Max attachments per email | 10 |
| Max emails per batch | 100 |
| List page size | default 20, max 100 |
