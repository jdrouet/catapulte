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

## Out of scope (for now)

Bounce and complaint ingestion, scheduled sends, recipient suppression lists, multi-tenant auth. Listed so they aren't mistaken for missing stories.
