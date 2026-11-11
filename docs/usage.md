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

If the server sets `CATAPULTE_HTTP_API_KEY` or any `CATAPULTE_API_KEYS` entry, every endpoint **except** the health
probes requires a bearer token:

```bash
curl http://localhost:3000/emails \
  -H "Authorization: Bearer $CATAPULTE_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{ ... }'
```

A missing or wrong token returns `401`. When no key is configured, the API is
unauthenticated and every caller is a trusted administrator (only safe behind a
trusted network boundary).

Named keys are scoped: a key that is not an admin only lists its own emails and
events, and its idempotency keys do not collide with other keys'. A named key
with `_SENDER_DOMAINS` gets `403` for a sender outside that list.

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
| `reply_to` | string, `{ name, address }`, or an array of either | `Reply-To` mailboxes, validated like the sender (see [Reply-To and custom headers](#reply-to-and-custom-headers)) |
| `headers` | object of string to string | custom headers (see [Reply-To and custom headers](#reply-to-and-custom-headers)) |
| `unsubscribe` | object `{ url?, mailto? }` | `List-Unsubscribe` and one-click unsubscribe (see [Unsubscribe](#unsubscribe)) |
| `tags` | array of strings | labels for filtering (`GET /emails?tag=`); up to 5, each 1-64 characters from `a-z`, `0-9`, `_`, `.` and `-` |
| `metadata` | object of string to string | free-form pairs echoed in every lifecycle event; up to 10 keys, keys 1-64 bytes, values up to 1024 bytes |

### Reply-To and custom headers

`reply_to` takes one mailbox or a list of at most 10. Each entry follows the sender rules:
a bare address, `Name <address>`, or `{ "name", "address" }`, and display names with
control characters are rejected with `400`. The formatted `Reply-To` header must not
have a line over 998 characters.

`headers` is an object of header name to value. An invalid set returns `400` (a per-item
`rejected` in a batch) on every submission path (JSON, multipart, batch and NATS):

- at most 10 headers;
- names use printable ASCII (33-126) without `:`, at most 76 bytes, and are unique
  ignoring case;
- values are at most 998 bytes and contain no CR, LF or other control characters (a tab is
  allowed); non-ASCII values are RFC 2047 encoded;
- no formatted header line, name and `: ` included, may exceed 998 characters. Headers
  are folded only at spaces (not tabs), so an unbroken value longer than 993 characters
  (for a 3-character name) is rejected. The same limit applies to the `Subject`, `From`,
  `To`/`Cc` and `Reply-To` lines, and a violation returns `400`;
- a JSON object that repeats a header name is rejected with `400`;
- these names are reserved, compared case-insensitively: `From`, `To`, `Cc`, `Bcc`,
  `Subject`, `Date`, `Message-ID`, `MIME-Version`, `Content-*`, `Return-Path`,
  `DKIM-Signature`, `Reply-To`, `Sender`, `Received`, `Authentication-Results`,
  `Return-Receipt-To`, `Disposition-Notification-To`, `Resent-*`, and
  `List-Unsubscribe*`. Use `reply_to` for `Reply-To`. `Precedence`, `Auto-Submitted`
  and `X-Mailer` are allowed.

```json
{
  "reply_to": ["Support <help@example.com>"],
  "headers": { "X-Order-Id": "42", "In-Reply-To": "<abc@example.com>" }
}
```

### Unsubscribe

`unsubscribe` sets the `List-Unsubscribe` header (RFC 2369) and, when `url` is set, the
one-click header `List-Unsubscribe-Post: List-Unsubscribe=One-Click` (RFC 8058):

```json
"unsubscribe": { "url": "https://example.com/unsubscribe/42", "mailto": "unsub@example.com" }
```

At least one of `url` and `mailto` is required. `url` must be a valid `https` URL and
`mailto` a valid email address; otherwise the request fails with `400`, as it does when the
resulting header line exceeds 998 characters. The header lists `url` first, then `mailto`.
The `mailto` local part may only contain letters, digits and `. _ + -`; an international domain is converted to punycode. Catapulte only sets the headers; your service must handle the unsubscribe request.

### Body variants

`body` is a tagged union on `kind`:

| `kind` | Fields | Description |
|--------|--------|-------------|
| `plain` | `text` and/or `html` (at least one) | a ready-made plain-text and/or HTML body |
| `mjml_inline` | `source` | raw MJML source, rendered with `variables` |
| `mjml_named` | `name` | a template pre-registered on the server, rendered with `variables` |
| `mjml_remote` | `url` | MJML fetched over HTTP (supports `mj-include`), rendered with `variables` |

### Template variables and escaping

Variables are HTML-escaped when they are rendered into MJML sources and `plain` HTML parts, so a value such as `<b>` appears as text. Plain-text parts are not escaped. Use the `safe` filter only for trusted markup, for example `{{ snippet|safe }}`.

Escaping does not validate URL schemes: a variable used in `href` or `src` can still hold a `javascript:` URL. The text part derived from `mj-preview` is decoded back to plain text and is not escaped. The email subject is not a template and is never interpolated.

Variables in MJML contexts that are not HTML have no correctness or security guarantee, and simple URLs work only incidentally:

- `mj-include path`
- `mj-font href`, which becomes a CSS `@import url(...)`
- `mj-style`
- `mj-raw` containing script or style

`|safe` turns escaping off but does not validate URLs or escape CSS or script, so a value used there must be trusted and valid for that context. For example, an apostrophe can still break CSS. Do not put untrusted variables in these contexts.

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
    "subject": "Hi Alice",
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

`content_type` must be a valid MIME type (parameters such as `; charset=utf-8` are fine),
and `filename` is reduced to its last path segment (after `/` or `\`); an empty name,
one over 255 bytes, or one with control characters is rejected with `400`. A multipart
part without a content type defaults to `application/octet-stream`.

Remote URLs are fetched server-side subject to the operator's allow-list; an
unreachable or disallowed URL fails the submission with `400`.

#### Inline images (`content_id`)

Set `content_id` on an attachment to send it as an inline part that HTML can reference as
`<img src="cid:logo">`. The value must be 1-255 ASCII graphic characters, without
whitespace, `<` or `>`, and unique within the email; otherwise the request fails with
`400`. In multipart, set it with the part's `Content-ID` header (one surrounding `<>` is
stripped). Without an HTML body, inline attachments are sent as regular attachments.

```json
{ "filename": "logo.png", "content_type": "image/png", "inline_base64": "<base64>", "content_id": "logo" }
```

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

If the earlier submission with that key never completed (still in progress, or its
cleanup failed), Catapulte returns `409` with `submission with this idempotency key is
incomplete; retry later`. In a batch that item is `rejected`.

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
| `tag` | only emails carrying this tag |
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
      "payload": {
        "sender_name": "primary",
        "correlation_id": "order-12345",
        "smtp_response": "250 2.0.0 Ok: queued as 4ABC12D3",
        "message_id": "<018f4e3c-2d1a-7b3c-8f00-1234567890ab@example.com>"
      },
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
  "payload": {
    "sender_name": "primary",
    "correlation_id": "order-12345",
    "smtp_response": "250 2.0.0 Ok: queued as 4ABC12D3",
    "message_id": "<018f4e3c-2d1a-7b3c-8f00-1234567890ab@example.com>"
  }
}
```

| `event_type` | Meaning | `payload` fields |
|--------------|---------|------------------|
| `queued` | accepted and enqueued | `correlation_id` |
| `sending` | a delivery attempt is starting | `attempt`, `correlation_id` |
| `delivery.succeeded` | accepted by the upstream SMTP server | `sender_name`, `correlation_id`, `smtp_response`, `message_id` |
| `retrying` | attempt failed, will retry | `attempt`, `reason`, `error_class`, `sender_name`, `correlation_id` |
| `delivery.failed` | retries exhausted, or no recipient left after suppression (`error_class` `suppressed`) | `attempt`, `reason`, `error_class`, `sender_name`, `correlation_id` |
| `bounced` | a remote server reported a bounce (see [ingestion](../readme.md#bounce-and-complaint-ingestion)); informational, never changes the status | `hard`, `recipient`, `diagnostic_code`, `correlation_id` |
| `complained` | a mailbox provider reported a spam complaint; `verified` is true only when the report carried a valid signed return path; informational | `recipient`, `verified`, `correlation_id` |
| `suppressed` | recipients were dropped because they are on the suppression list; informational, never changes the status | `addresses`, `correlation_id` |

Every event also carries the email's `tags` and `metadata` in `payload`, but only when the
email has some; events of an email without them have the payload shown above. `GET /emails`
returns the same two fields on each email, again only when set.

On `delivery.succeeded`, `smtp_response` is the relay's reply as `"{code} {text}"`
(for example `250 2.0.0 Ok: queued as 4ABC12D3`), cut to at most 256 characters (the
ellipsis included), and
`message_id` is the `Message-ID` header Catapulte set, `<{email id}@{sender domain}>`
with the domain in lower-case ASCII (an internationalised domain is written in its
punycode form). Use them to find the message in the relay's logs or a
later bounce. The `Message-ID` depends only on the email id and sender, so retries and
duplicate deliveries of the same email under at-least-once delivery carry the same value.

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

Always include an `idempotency_key` in NATS submissions. Failed submissions are
negatively acknowledged and redelivered; the key lets a redelivery recover an email
whose queue publish was left uncertain instead of creating a second one.

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

## Templates

Admin keys manage named MJML templates that emails reference with `{"kind": "mjml_named", "name": "..."}`.
Every route needs an admin key and returns `403` for any other key.

- `GET /templates` lists the latest version of each stored template (`name`, `version`, `created_at_ms`).
- `GET /templates/{name}` returns the latest version with its `source`, or `404`.
- `PUT /templates/{name}` with `{"source": "<mjml>...", "validation_variables": {...}}` validates the
  source and stores it as the next version (`validation_variables` is optional). Sends use the latest
  version.
- `DELETE /templates/{name}` removes every version and returns `204`, or `404`.
- `POST /templates/{name}/preview` with `{"variables": {...}}` returns `{"html", "text"}` rendered
  exactly as a send would render it, without sending anything.

Names are 1-128 characters from `a-z`, `0-9`, `_`, `.` and `-`, and cannot start with a dot. The
source is limited to 512 KiB, and neither the source nor the variables may contain NUL.

**Precedence.** The files in `CATAPULTE_RESOLVER_TEMPLATES_DIR` are authoritative. A name that exists on
disk is always rendered from disk, and `PUT` and `DELETE` for it return `409`. Stored templates only
serve names the files do not have.

**Validation.** `PUT` renders the source with `validation_variables` (default empty) using the send
engine's HTML escaping, then compiles the result as MJML. A failure returns `400` with the message.
Validation is lenient about missing data: a missing variable or a missing nested attribute such as
`{{ customer.name }}` prints as empty and iterates as empty, so the MJML structure still validates.
Operations that need a real value still fail without data: arithmetic (`{{ total + 1 }}`),
comparisons used as a value, and filters that need a type (`|round`, `|int`, `|join` on a non-sequence).
Pass representative `validation_variables` for those. A passing validation proves syntax and MJML
structure, not that the template renders for every input; sends are strict, and a missing nested
attribute fails at send time.

Every render (send, validation and preview) has a fixed budget of work. A template that exceeds it, for
example a runaway nested loop, fails permanently at send time (`delivery.failed` with `error_class`
`template_interpolate`, no retry) and is rejected with `400` on upload or preview. A table of 500 rows uses
about 3% of the budget.

Rendering also writes at most 1 MiB of output per part, and a filter that turns a list or map into text
fails once the text would pass 1 MiB. To keep that bound, the `format`, `pprint` and `indent` filters are
not available, and `replace` takes no `count`.

These limits do not yet cover every template operation: string concatenation with `~` and comparisons of
large nested lists can still use excessive CPU and memory. Until rendering moves into a resource-limited
process (tracked in #815), only give API keys to callers you trust not to submit hostile templates.

## Suppression list

Addresses on the suppression list never receive mail. Every `/suppressions` route needs an
admin key and returns `403` for any other key. Addresses are stored as lowercased bare
addresses, and a recipient matches on that form in `to`, `cc` and `bcc`.

- `GET /suppressions?reason=&address=&limit=&offset=` lists entries, newest first. `reason`
  is `bounce`, `complaint`, `unsubscribe` or `manual`; `address` matches a substring.
- `POST /suppressions` with `{"suppressions": [{"address": "a@b.c", "reason": "manual"}]}`
  adds up to 100 entries (`reason` defaults to `manual`) and returns `204`. An address that is
  already listed keeps its first entry.
- `DELETE /suppressions/{address}` returns `204`, or `404` when the address is not listed.

On submit, suppressed recipients are dropped. If none are left, the request fails with `422`
(a per-item rejection in a batch, and a discarded message on inbound NATS) and nothing is
stored. A resubmit with an idempotency key that already identifies an accepted email returns
that email's id even if a recipient was suppressed afterwards. Dropped recipients are reported
in a `suppressed` lifecycle event, which does not change the email's status and is not re-emitted when a retry replays an interrupted submission. The worker checks
the list again before sending: it drops newly suppressed recipients, and if none remain it
fails the email with `delivery.failed` and `error_class` `suppressed`.

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
| `400` | malformed JSON/multipart, validation failure (sender/recipients/reply_to/headers/body/attachment), bad UUID, unreachable/disallowed remote attachment, batch over 100 |
| `401` | missing/invalid bearer token |
| `403` | the sender's domain is not in the operator's `CATAPULTE_ALLOWED_SENDER_DOMAINS` list (exact domain match, no subdomains), including an idempotent retry whose sender is no longer allowed, or one that would republish a stored original whose sender is no longer allowed; in a batch the item is rejected instead |
| `409` | idempotency key matches a submission that never completed; retry later |
| `422` | every recipient is on the suppression list |
| `500` | storage / queue / attachment-store failure |

## Limits

| Limit | Value |
|-------|-------|
| Max request body | 352 MiB |
| Max envelope JSON (multipart `envelope` part) | 1 MiB |
| Max size per attachment | 25 MiB |
| Max attachments per email | 10 |
| Max message size | 25 MiB by default (`CATAPULTE_MAX_MESSAGE_BYTES`) |
| Max emails per batch | 100 |
| Tags per email | 5 (1-64 characters each) |
| Metadata keys per email | 10 (keys 1-64 bytes, values up to 1024 bytes) |
| List page size | default 20, max 100 |

The message size limit is checked twice. At submit, Catapulte estimates the size as
`(attachment bytes + inline body length) × 4/3` and returns `400` (a per-item
`rejected` in a batch) when it exceeds the limit; the estimate excludes template and
variable growth, rendering, MIME headers, boundaries and line wrapping, so it is an
early rejection, not a guarantee. The final check measures the rendered message just
before sending: a message over the limit is never sent and ends as `delivery.failed`
with `error_class` `rejected`. To avoid relay-side rejections, set the limit at or
below your relay's own limit, minus any headers the relay adds.
