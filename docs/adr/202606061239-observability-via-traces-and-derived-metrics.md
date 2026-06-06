---
status: accepted
date: 2026-06-06
decision-makers: Jeremie Drouet
---

# Observability via OpenTelemetry traces with collector-derived metrics

## Context and Problem Statement

Catapulte needs production observability. Operators must be able to monitor every
adapter (storage, queue, attachment store, SMTP transport, event publisher,
template resolver, ...) as well as every upstream SMTP sender, and the collection
backend must be selectable at runtime: a Prometheus scrape, or an OpenTelemetry
Collector reached over OTLP (HTTP or gRPC).

The constraint that shapes the answer is the hexagonal rule in `CLAUDE.md`: the
`domain/` crate must not depend on any I/O crate, and must not depend on
`opentelemetry`. Instrumentation therefore cannot be expressed as a domain
concern without breaking the dependency direction.

One enabling fact tilts the design. `tracing` is *already* an accepted dependency
of `domain` (`domain/Cargo.toml`, line 17) because it is a logging facade rather
than an I/O crate; the actual telemetry export is wired only in the binary
(`binary/src/main.rs` already initializes `tracing_subscriber::fmt`). Adding spans
across `domain` and the adapters therefore carries no architectural cost: the
facade is permitted everywhere. By contrast, adding OpenTelemetry metric
instrumentation would require pulling `opentelemetry` into a new crate and writing
per-port wrapper types to keep it out of the adapters and the domain.

The workload also matters: Catapulte is SMTP-bound and low-QPS (seconds per send,
a handful of operations per email), so full-fidelity span export is affordable.

The question is how to deliver per-adapter and per-sender RED metrics, plus causal
request tracing, without polluting `domain` and without a large, long-lived body
of instrumentation code.

## Decision Drivers

- Keep `domain/` free of `opentelemetry` and any I/O crate (hexagonal rule).
- Monitor every adapter and every upstream SMTP sender.
- Runtime-configurable collection: Prometheus, or OTLP over HTTP or gRPC.
- Provide causal debugging of the submit -> enqueue -> worker dequeue -> SMTP
  deliver chain, not only aggregate counters.
- Minimize application-side instrumentation code and its long-term maintenance.
- The low-QPS workload makes per-operation span export viable.

## Considered Options

1. In-app metrics via per-port decorators: a new `catapulte-telemetry` crate with
   roughly ten decorators, one per driven port, plus edge metrics; the application
   exposes a Prometheus pull endpoint and/or pushes OTLP metrics directly.
2. A `MetricsRecorder` driven port in `domain/` that domain services call.
3. Hybrid, traces-led: instrument the application with OpenTelemetry traces via
   `tracing` + `tracing-opentelemetry`, export OTLP (HTTP or gRPC, runtime
   selectable) to an OpenTelemetry Collector, and derive per-operation RED metrics
   in the Collector's spanmetrics connector (per-adapter via span name, per-sender
   via span attributes); keep a thin in-app OTel metrics layer for gauges only.

## Decision Outcome

Chosen option: **Option 3, the hybrid traces-led approach**.

It carries the lowest application-side code and maintenance burden. Spans add
effectively zero hexagonal friction because `tracing` is already permitted
everywhere, including `domain`. A single instrumentation effort yields traces,
metrics, and request causality together rather than three separate efforts, and a
low-QPS mailer can afford to export every span.

Concretely, the decision means:

- The application exports **only OTLP**. The protocol (`grpc` or `http/protobuf`)
  is selectable at runtime, alongside endpoint and headers. Prometheus is fed by
  the Collector (Prometheus scrapes the Collector), never by the application
  directly. **This narrows the original requirement.** The initial ask was
  "configure a Prometheus scrape *or* an OTLP Collector at runtime"; this decision
  changes it to "the application emits OTLP only, and Prometheus is obtained *via*
  the Collector." Operators who want Prometheus must run a Collector. This is a
  deliberate requirement change, not merely a consequence, and is accepted because
  the Collector is the natural integration point for a fleet and because keeping a
  second, in-app metrics pipeline alive purely for a collector-less Prometheus
  scrape reintroduces most of the cost this decision exists to avoid.
- The OpenTelemetry Collector is accepted as a **hard dependency for metrics**.
  The application speaks OTLP only; the Collector re-exports to Prometheus or any
  other backend.
- Per-operation RED metrics (`calls`, `duration`) are derived in the Collector's
  spanmetrics connector: per-adapter dimensioned from the span name, per-sender
  dimensioned from a bounded span attribute.
- A small set of **observable gauges remains in-app**, because they are
  point-in-time and not derivable from spans. Each gauge's source is named so it
  is not a placeholder:
  - **queue depth / backlog** (`catapulte.queue.pending`): a non-domain,
    best-effort `pending() -> Option<u64>` capability on the binary's
    `QueueAdapter` enum (`binary/src/queue.rs`) - SQL `count` for the storage
    backends, in-memory length for `MemoryQueue`, JetStream consumer `num_pending`
    for NATS, `None` where a backend cannot answer cheaply - read from an OTel
    observable-gauge callback. Explicitly **not** a new domain port on `EmailQueue`.
  - **per-sender quota usage** (`catapulte.sender.quota_usage`): sourced from the
    existing domain port `SenderUsage::get_stats()` (`domain/src/port/sender_usage.rs`),
    invoked from an observable-gauge callback in the binary. No new domain surface.
  - **oldest-pending-message age**: backend-specific and lower priority; shipped
    only for backends that can report it cheaply (e.g. min `created_at` for the SQL
    queues) and otherwise **deferred**, since it is not uniformly expressible.

- **Trace-context propagation** is required for the "one trace" claim to hold.
  Spans created in the HTTP request do not automatically continue in the worker:
  the worker starts from `dequeue` and today carries only `correlation_id` inside
  the envelope. To stitch the chain, the submit path injects the **W3C
  `traceparent`** (and `tracestate`) into the queued message - as queue metadata
  for the SQL/memory backends and as **NATS message headers** for the NATS queue
  and the inbound-NATS path - and the worker/consumer extracts it to start its span
  as a child of the producer's span. Without this, the system emits several
  disconnected traces, which is acceptable as an interim state but is not "one
  trace". The Confirmation criterion below depends on this propagation being in
  place.
- Per-sink event-publisher visibility comes for free from per-sink spans.
  `PublisherAdapter` (`binary/src/publisher.rs`) `tokio::join!`s the storage,
  webhook, and NATS sinks but only propagates the storage error; webhook and NATS
  failures are logged and swallowed, so the adapter returns `Ok(())`. An aggregate
  metric would therefore hide a failing sink, but a span per sink records its own
  error status regardless, so spanmetrics yields accurate per-sink RED.
- Spans instrument: the submit use case, enqueue, the worker `process_one`
  (`adapter/inbound-worker/src/worker.rs`, deliberately not the idle dequeue
  polls), each adapter operation, and each per-sink publish. Inbound HTTP already
  carries a `TraceLayer` and some `#[tracing::instrument]`. Note that the
  `TraceLayer` provides span *creation* for incoming HTTP requests, not W3C
  remote-parent *extraction*; external HTTP-client to catapulte trace continuity
  is a separate follow-up. Phase 2b delivers internal submit -> queue -> worker
  continuity via `TraceCarrier` propagation through the SQL and memory queue
  backends.
- Configuration: `CATAPULTE_`-prefixed env vars take precedence, with the standard
  `OTEL_*` vars honored as a fallback. `service.name` and `service.version` are set
  as resource attributes. Tracer and meter provider shutdown performs a bounded
  flush, mirroring the existing 30s task-drain timeout in
  `binary/src/lib.rs run_with_shutdown`, so a hung collector cannot block process
  exit.
- Subscriber composition: the existing console logging
  (`binary/src/main.rs`, `tracing_subscriber::fmt`) and the new OTLP trace export
  are attached as **two layers on a single `tracing_subscriber` registry** with one
  `init()`. There is no second pipeline; the OTel layer is added beside the `fmt`
  layer, both fed by the same `EnvFilter`.
- Correlation: the existing `correlation_id` (persisted on the email and carried on
  lifecycle events) and the OpenTelemetry trace/span ids are **distinct
  identifiers**. `correlation_id` is recorded as a **span attribute** so a trace can
  be found from a caller-supplied id and vice versa; it is not overloaded onto the
  trace id and is not put into baggage.
- Cardinality discipline: email ids, recipients, correlation ids, and raw URLs are
  never placed in span attributes that become metric dimensions; route templates
  and bounded sender names only.

### Consequences

Good:

- Minimal application code: no roughly-ten decorator structs to write and maintain.
- `domain/` stays pure; no `opentelemetry` dependency crosses the hexagon.
- Traces and metrics both fall out of one instrumentation effort.
- Per-adapter and per-sender RED metrics, plus causal request tracing across the
  full submit -> enqueue -> worker -> deliver chain.
- A single OTLP export path with runtime gRPC / HTTP selection.

Bad / risks:

- The Collector becomes a required component for metrics: a real operational
  dependency.
- There is no collector-less, direct-Prometheus path for RED metrics.
- For accurate derived metrics the SDK must export 100% of spans, and the Collector
  must run spanmetrics *before* any tail-sampling; head-sampling in the application
  would undercount the metrics.
- Collector-derived metrics carry operational costs that in-app counters would not:
  spanmetrics counters **reset when the Collector restarts**, so queries must be
  `rate()`/increase-aware rather than reading raw counters; at low QPS a series can
  look **stale** between sparse updates and after a flow stops; and the connector's
  **dimension cardinality cache must be explicitly bounded** in Collector config to
  avoid unbounded memory growth. The metric **temporality** (cumulative vs delta)
  must be chosen to match the downstream backend (cumulative for Prometheus).
- A thin in-app gauge-metrics layer is still required (it cannot be removed
  entirely).
- The "one trace" outcome depends on W3C trace-context propagation through the
  queue and NATS headers (see the decision body); until that lands, the pipeline
  emits several disconnected traces.
- Dependency and version churn in the OpenTelemetry Rust and `tracing-opentelemetry`
  stack must be pinned to a mutually compatible set.
- Per-operation span volume has a cost, mitigated here by the low-QPS workload.

Neutral:

- The choice of Prometheus versus another metrics backend moves out of application
  configuration and into Collector configuration.

### Confirmation

- A docker-compose example wires an `otel/opentelemetry-collector` with the
  spanmetrics connector and a Prometheus (or OTLP) exporter.
- A single submitted email produces one trace spanning HTTP -> enqueue -> worker ->
  SMTP deliver.
- The `calls` and `duration` metrics appear dimensioned per-adapter and per-sender.
- The queue-depth gauge is exported.
- The standard gate stays green:
  `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt`,
  `cargo machete`, and the test suites.

## Pros and Cons of the Options

### Option 1 - In-app metrics via per-port decorators

A `catapulte-telemetry` crate provides one decorator per driven port plus edge
metrics; the application exposes a Prometheus endpoint and/or pushes OTLP metrics.

- Good: the Collector is optional; Prometheus can scrape the process directly.
- Good: counts are independent of any span sampling decision.
- Bad: introduces an `opentelemetry` dependency and roughly ten decorator structs
  to write, wire, and maintain.
- Bad: no request causality, only aggregate metrics.
- Bad: because `PublisherAdapter` swallows per-sink failures, the decorators must
  be applied *below* the fan-out (one per inner sink) to avoid recording `ok` while
  a sink failed; an explicit, easy-to-get-wrong wiring constraint.
- Bad: gauges (queue depth, quota) still need a separate mechanism on top of the
  decorators.

### Option 2 - A `MetricsRecorder` driven port in `domain/`

Domain services call a `MetricsRecorder` port to emit observability data.

- Good: centralizes instrumentation at the call sites that own the logic.
- Bad: pollutes `domain` with an observability concern and threads a recorder
  through services that have no business knowing about telemetry. This is squarely
  against the hexagonal rule that `domain` carries no I/O or telemetry concern.
  Rejected on those grounds.

### Option 3 - Hybrid, traces-led (chosen)

Instrument with `tracing` spans exported as OTLP traces, derive metrics in the
Collector's spanmetrics connector, and keep a thin in-app layer for gauges only.

- Good: lowest application code; `domain` stays pure because `tracing` is already
  permitted there.
- Good: traces, metrics, and causality from one instrumentation pass.
- Good: per-sink and per-sender accuracy without touching domain wiring.
- Bad: requires the Collector for metrics and full span export ahead of any
  tail-sampling, plus pinned OTel dependency versions, as captured in the
  consequences above.

## More Information

An earlier planning note proposed Option 1 (per-port decorators); the decision
recorded here reverses its core mechanism from in-app metric decorators to
collector-derived metrics over traces, while retaining its survey of ports, sinks,
and cardinality rules as input.

The implementation is expected to land in high-level phases:

1. Traces plus OTLP export plus runtime configuration (protocol, endpoint, headers,
   resource attributes, bounded shutdown flush, `CATAPULTE_*`-over-`OTEL_*`
   precedence).
2. Span coverage across the worker `process_one`, each adapter operation, and each
   per-sink publish, including **W3C trace-context propagation** through the queued
   message and NATS headers so the producer and worker spans form one trace, with
   `correlation_id` recorded as a span attribute.
3. The thin in-app gauge-metrics layer, including queue depth via the binary's
   `QueueAdapter::pending()` capability.
4. The Collector docker-compose example plus a readme configuration table.
5. Optional tail-sampling tuning once volume and cost are observed in practice.

This record is about the decision and its rationale; the phases above are a
high-level map, not a line-by-line implementation plan.
