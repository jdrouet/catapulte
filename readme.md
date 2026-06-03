# Catapulte

Make sending email easy.

## User stories

Within each persona, stories are ordered by priority (most important first).

### API consumer

- [ ] As an API consumer, I can ask an email (text or html) to be sent through a SMTP server, and get back a tracking id, so that I don't have to manage SMTP and retries myself.
- [ ] As an API consumer, I can ask an email to be sent from inline mjml plus variables, so that I keep template sources in my own repo.
- [ ] As an API consumer, I can ask an email with attachments to be sent through a SMTP server, so that I can send invoices, receipts or reports.
- [ ] As an API consumer, I can list emails I previously submitted with filters (status `queued` / `sent` / `failed`, time range, recipient, template, tracking id), paginated, so that I can check delivery state and debug without keeping my own mirror of the data.
- [ ] As an API consumer, I can pass an idempotency key on submission, so that retrying a failed request doesn't send the email twice.
- [ ] As an API consumer, I can submit a batch of emails in a single request and get back one tracking id per email, so that I can fan out a campaign without N round-trips. Partial acceptance is allowed: per-email validation errors are returned alongside the accepted ids.
- [ ] As an API consumer, I can ask an email to be sent from a pre-registered template name + variables, so that callers don't ship template bytes on every request.
- [ ] As an API consumer, I can ask an email to be sent from a remote mjml template fetched over http (with `mj-include`) + variables, so that templates can live in a CMS or shared repo.
- [ ] As an API consumer, I can list the lifecycle events for emails I submitted (`queued`, `sending`, `delivery.succeeded`, `delivery.failed`, `retrying`), with filters (tracking id, event type, time range), paginated, so that I can debug a delivery without subscribing to the live event stream.

### Operator

- [ ] As an operator, I can configure multiple SMTP servers with routing rules, so that I can fail over or split traffic per sender domain.
- [ ] As an operator, I can set per-server quotas (rate and daily cap), so that I stay within provider limits without dropping traffic.
- [ ] As an operator, I can list lifecycle events across all submissions (not scoped to one consumer) with filters (event type, time range, upstream server, error class), paginated, so that I can investigate incidents and audit traffic.
- [ ] As an operator, I can expose multiple ingress transports for API consumers (HTTP for request/response CRUD, NATS for fire-and-forget submissions, more later), so that consumers can pick the integration style that fits their stack. Each transport can be enabled or disabled independently. NATS submissions don't return a tracking id synchronously: the consumer supplies a correlation id and observes outcome via lifecycle events.

### Event subscriber

- [ ] As an event subscriber, I receive a `delivery.succeeded` event when an email is accepted by the upstream SMTP, so that I can update my own state.
- [ ] As an event subscriber, I receive a `delivery.failed` event after retries are exhausted, so that I can alert or compensate. The event carries the last error and the attempt count.
- [ ] As an event subscriber, I receive events over whichever transport the operator has enabled globally (webhook to a configured URL, or NATS on a configured subject), so that I can plug catapulte into the bus my stack already speaks without managing per-subscription transport config.


## Quick Start

The easiest way to run Catapulte is using Docker Compose. Several examples are provided in the [compose](./compose) directory:

- **Local Development**: `docker-compose -f compose/local-dev.yml up`
  Starts Catapulte with an in-memory database and [Mailpit](https://github.com/axllent/mailpit) for local SMTP testing.
- **SQLite (Persistent)**: `docker-compose -f compose/sqlite.yml up`
  Starts Catapulte with a persistent SQLite database.
- **Postgres & NATS**: `docker-compose -f compose/postgres-nats.yml up`
  A more robust setup using Postgres for storage and NATS for the email queue and events.


### Verifying the Setup

You can run an automated smoke test against all compose configurations by running:

```bash
just test-compose
```

This script will bring up each configuration, submit a test email, verify it reached Mailpit, and then shut down the services.

## Configuration

All configuration is done via environment variables.

### General

| Variable | Description | Default |
|----------|-------------|---------|
| `CATAPULTE_GC_SWEEP_INTERVAL_SECS` | Interval in seconds between garbage collection sweeps | `3600` |
| `CATAPULTE_GC_GRACE_PERIOD_SECS` | Minimum age for data to be eligible for garbage collection | `3600` |

### Storage Backend

| Variable | Description | Default |
|----------|-------------|---------|
| `CATAPULTE_STORAGE_BACKEND` | Storage engine: `sqlite` or `postgres` | `sqlite` |
| `CATAPULTE_SQLITE_URL` | SQLite connection string (e.g. `sqlite://catapulte.db`) | - |
| `CATAPULTE_POSTGRES_URL` | Postgres connection string (e.g. `postgres://user:pass@host/db`) | - |

### Inbound Transports

#### HTTP
| Variable | Description | Default |
|----------|-------------|---------|
| `CATAPULTE_HTTP_ADDRESS` | Bind address for the HTTP server | - |

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
| `CATAPULTE_SENDER_{NAME}_HOST` | **(Required)** SMTP hostname | - |
| `CATAPULTE_SENDER_{NAME}_PORT` | SMTP port | `587` |
| `CATAPULTE_SENDER_{NAME}_USERNAME` | SMTP username | - |
| `CATAPULTE_SENDER_{NAME}_PASSWORD` | SMTP password | - |
| `CATAPULTE_SENDER_{NAME}_TLS` | `starttls`, `tls`, or `none` | `starttls` |
| `CATAPULTE_SENDER_{NAME}_PRIORITY` | Lower numbers are tried first | `100` |
| `CATAPULTE_SENDER_{NAME}_QUOTA_COUNT` | Max emails allowed in range | - |
| `CATAPULTE_SENDER_{NAME}_QUOTA_RANGE` | `hourly`, `daily`, `weekly`, or `monthly` | - |
| `CATAPULTE_SENDER_{NAME}_MATCH_DOMAIN` | Optional domain to strictly route traffic for | - |

### Email Queue

| Variable | Description | Default |
|----------|-------------|---------|
| `CATAPULTE_QUEUE_BACKEND` | `storage`, `memory`, or `nats` | `storage` |

#### NATS Queue (if backend is `nats`)
| Variable | Description | Default |
|----------|-------------|---------|
| `CATAPULTE_QUEUE_URL` | **(Required)** NATS server URL | - |
| `CATAPULTE_QUEUE_STREAM` | JetStream stream name | `CATAPULTE_EMAILS` |
| `CATAPULTE_QUEUE_SUBJECT` | JetStream subject | `catapulte.emails.queued` |
| `CATAPULTE_QUEUE_CONSUMER` | Pull consumer name | `catapulte-worker` |
| `CATAPULTE_QUEUE_ACK_WAIT_SECS` | Redelivery timeout | `30` |
| `CATAPULTE_QUEUE_MAX_DELIVER` | Maximum delivery attempts | `3` |
| `CATAPULTE_QUEUE_BACKOFF` | Comma-separated retry backoff steps in seconds | `30,60,120` |

### Event Publishers (Observability)

| Variable | Description | Default |
|----------|-------------|---------|
| `CATAPULTE_WEBHOOK_URL` | URL to POST lifecycle events to | - |
| `CATAPULTE_WEBHOOK_TIMEOUT_MS` | Webhook call timeout | `5000` |
| `CATAPULTE_NATS_EVENTS_URL` | NATS server for event publishing | - |
| `CATAPULTE_NATS_EVENTS_SUBJECT` | Subject for lifecycle events | `catapulte.lifecycle` |

### Template Management

#### Template Resolver
| Variable | Description | Default |
|----------|-------------|---------|
| `CATAPULTE_RESOLVER_ALLOWED_DOMAINS` | Allowed domains for remote MJML fetching | - |
| `CATAPULTE_RESOLVER_TEMPLATES_DIR` | Directory containing `.mjml` templates | - |

#### MJML Include Loader
| Variable | Description | Default |
|----------|-------------|---------|
| `CATAPULTE_INCLUDE_LOADER_FS_ROOT` | Local root for `<mj-include>` | - |
| `CATAPULTE_INCLUDE_LOADER_HTTP_ALLOW` | Allowed origins for HTTP includes | - |
| `CATAPULTE_INCLUDE_LOADER_HTTP_DENY` | Blocked origins for HTTP includes | - |

### Attachments

#### Attachment Store
| Variable | Description | Default |
|----------|-------------|---------|
| `CATAPULTE_ATTACHMENT_BACKEND` | Currently only `fs` is supported | `fs` |
| `CATAPULTE_ATTACHMENT_FS_ROOT` | Directory for attachment storage | - |

#### Attachment Fetcher
| Variable | Description | Default |
|----------|-------------|---------|
| `CATAPULTE_ATTACHMENT_FETCHER_ALLOWED_DOMAINS` | Allowed domains for fetching | - |
| `CATAPULTE_ATTACHMENT_FETCHER_ALLOW_HTTP` | Allow non-HTTPS fetches | `false` |
| `CATAPULTE_ATTACHMENT_FETCHER_MAX_BYTES` | Max size per attachment | `25MiB` |
| `CATAPULTE_ATTACHMENT_FETCHER_FETCH_TIMEOUT_MS` | Fetch timeout | `30000` |

## Out of scope (for now)

Bounce and complaint ingestion, scheduled sends, recipient suppression lists, multi-tenant auth. Listed so they aren't mistaken for missing stories.
