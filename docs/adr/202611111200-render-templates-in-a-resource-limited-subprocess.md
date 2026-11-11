---
status: proposed
date: 2026-11-11
decision-makers: Jeremie Drouet
---

# Render templates in a resource-limited subprocess

## Context and Problem Statement

Templates run inside the worker process. A fuel budget and a 1 MiB output cap (#815)
stop most runaway templates, but some MiniJinja operations are not metered:

- `~` (string concatenation) on nested lists that share structure expands exponentially.
- `==` on two separately built nested lists compares every path of the towers.
- Formatting a lazily built `a * 2` list expands it inside one formatter call.

A template under 1 KB can keep a worker busy for seconds to minutes or exhaust its memory,
and any API key can submit one as an `mjml_inline` body. The MJML engine (mrml) also has no
depth or include limits, so a deeply nested document can overflow the stack and abort the
whole process. These paths are inside the engines and cannot be wrapped through their
public APIs. The workspace forbids `unsafe_code`, so the usual `pre_exec` plus `setrlimit`
route is closed.

## Decision Drivers

- No template may make the worker exceed fixed CPU, memory or wall-clock limits.
- A limit hit must be a permanent failure; an infrastructure fault must stay retryable.
- Send, validation and preview must go through the same boundary.
- No new heavy dependency, and no `unsafe` code.

## Considered Options

1. **Keep tightening in-process limits.** Rejected: the unmetered paths live inside the
   VM, and a stack overflow or allocation failure aborts the process.
2. **A thread with a timeout.** Rejected: a thread cannot be killed or memory-limited.
3. **A fresh child process per job (chosen).** The kernel enforces CPU and address-space
   limits and the parent can kill the child.
4. **A warm pool of children.** Rejected for now: it adds state to isolate and to
   recycle, and the measured spawn cost is small (see Measurements).
5. **A WebAssembly sandbox.** Recorded as the upgrade path if a stronger boundary is
   needed. Out of scope here.

## Decision Outcome

### Process model

The binary re-executes itself as `catapulte __render-sandbox`. A synchronous `main`
dispatches this mode before any async runtime, telemetry or tracing setup. A job is one of
interpolate, validate or render (MJML). A plain-body render is the identity and spawns no
child. An MJML send is two jobs, a plain send is one, an upload is validate then render and
a preview is interpolate then render.

The parent writes one JSON request to the child's stdin and reads exactly one JSON outcome
from its stdout. Stdout is protocol only; stderr is inherited. Requests are capped at 4 MiB
and responses at 16 MiB. A result counts as a success only with exit status 0, one complete
valid outcome and end of file.

### Limits set by the child on itself

The child sets, in order: a parent-death signal (SIGKILL) with a check that its parent is
still the expected process, `RLIMIT_CORE` 0, `RLIMIT_CPU` (soft 2 s, hard 3 s) and
`RLIMIT_AS` (memory, default 256 MiB). On Linux a failure to set any limit is fatal.
It then asks the kernel to prefer it as an out-of-memory victim (`oom_score_adj` 1000,
best effort). The limits arrive as arguments, are read before stdin and are parsed
strictly. The child runs on a single-threaded runtime with at most two blocking threads.
Fuel and the output caps stay inside the child, so most bad templates still fail early and
cheaply. The final rendered output, literal text included, is bounded to 1 MiB per part.

### Parent responsibilities

- **Deadline.** `CATAPULTE_RENDER_TIMEOUT_MS` (default 7000) is one absolute deadline per
  stage. It covers admission wait, serialization, spawn, I/O and reaping. The kill is issued
  at the deadline, and reaping after a kill gets a fixed 1 s grace that is cleanup, not
  budget. A two-stage operation is bounded by 2 x (timeout + 1 s) of sandbox time. Startup
  refuses a configuration where `2 x (timeout + 1 s) + 5 s` exceeds the smaller of the HTTP
  request timeout and the 30 s shutdown drain. This bounds sandbox time, not the whole HTTP
  operation, which also includes repository I/O and SMTP.
- **Concurrency.** `CATAPULTE_RENDER_CONCURRENCY` (default: available parallelism) permits
  are shared by sends, validation and preview. Waiters apply backpressure and count against
  the stage deadline.
- **Environment.** The child starts with an empty environment plus an allowlist:
  `MALLOC_ARENA_MAX=2` (glibc only), the TLS certificate variables, the proxy variables and
  `REQUEST_METHOD` (so the proxy matcher behaves as in the parent). Proxy URLs can hold
  credentials, so the child environment is never logged. No `PATH`, log filter or
  Catapulte secret is passed.
- **Include policy.** MJML includes need a loader policy. The parent sends its effective,
  already validated include configuration inside the render job. It is taken from the
  application configuration, never from a request or a template. The per-fetch HTTP
  timeout is clamped to half the render timeout; several fetches can still exhaust the
  stage deadline, which is a permanent deadline hit.
- **Cancellation and shutdown.** A task owned by the sandbox, not the caller, runs each job.
  Dropping the caller cancels it; the owner kills and reaps the child and releases the
  permit, and never launches a child after the caller is gone. Owner tasks are tracked, and
  the application's shutdown drain cancels and waits for them. During orderly operation and
  shutdown every child is awaited. After a crash or a runtime abort, the parent-death signal
  and `kill_on_drop` are the fallback.
- **Parent-death signal semantics.** The signal follows the OS thread that spawned the
  child, so a child dies when that runtime worker thread retires, which in practice only
  happens at runtime shutdown. That kills the child and classifies as an infrastructure
  failure, which is accepted. Children are never spawned from blocking threads.

### Classification

| Observed | Error | Retry |
|---|---|---|
| The parent killed the child at the deadline while it was running | `BudgetExceeded` | permanent |
| The response passed its size cap | `BudgetExceeded` | permanent |
| The child returned a typed budget outcome (fuel or output cap) | `BudgetExceeded` | permanent |
| The child died from SIGXCPU | `BudgetExceeded` | permanent |
| Any other signal (allocation failure or stack overflow abort, OOM kill, external kill, segfault) | `Infrastructure` | transient |
| Setup failure, non-zero exit, malformed, missing or trailing response, spawn or I/O error, admission timeout | `Infrastructure` | transient |
| Exit 0 with a typed failure message | engine (interpolate) or MJML (render) error | as before |

The final classification is decided after the exit status is known, in this order: SIGXCPU,
then a deadline kill decided while the child was still running, then a response overflow,
then a valid complete outcome, otherwise infrastructure. If the deadline and completion are
both ready, the child's own result wins. The stage decides the error type, and error
classes stay `template_interpolate` and `template_render`. Template management maps an
infrastructure failure to 503 and every evaluation failure to 400.

An allocation failure aborts the child, so it is indistinguishable from other aborts and is
treated as infrastructure (retried up to the worker's attempt limit). A template that
reliably does this costs three failed children, not a worker.

### Memory envelope

The worst case is the parent baseline plus, per permit, the child's address-space limit
plus a 4 MiB request, a 16 MiB response and about 16 MiB decoded. "The worker survives"
holds only with that headroom; `oom_score_adj` is a preference, not a guarantee, and the
address-space limit is virtual, not resident.

## Consequences

- Every render costs a process spawn. The measured latency is in Measurements.
- Cancelling a request never leaves a child running, and a stuck child costs at most the
  deadline plus the 1 s grace.
- The release image needs the same binary at run time, which it already has. No Dockerfile
  or CI change is needed.
- The sandbox is a resource boundary, not a privilege boundary: the child has the parent's
  user and file access, and its network access follows the include policy.

## Confirmation

Integration tests drive the real binary against each hostile pattern, the classification
precedence, cancellation, parent death, runtime shutdown and large IPC, and the end-to-end
tests cover send, upload and preview. A startup self-test renders a trivial template through
the sandbox and refuses to start when it fails.

## More Information

Follows #815 and implements #816.

### Measurements

Measured on one Linux development machine with the debug test build and a release build.
Treat the numbers as orders of magnitude.

**Spawn cost.** One child round trip for a trivial template (spawn, request, render,
response, exit):

| Build | Samples | Median | p99 |
|---|---|---|---|
| debug (integration test through the sandbox) | 60 | 3.4 ms | 4.8 ms |
| release (child only, driven from a script) | 300 | 3.4 ms | 10.2 ms |

The cost is small against the 7 s deadline, which is why a warm pool is not needed yet.

**Memory.** The peak virtual size (`VmPeak`, sampled while the child runs, so a lower bound)
of the debug build was 61 MiB for a 5,000-row table and 61 MiB for a 3.4 MiB request with a
0.9 MiB response. The release build peaked at about 32 MiB for a trivial job. The default
limit of 256 MiB is above four times the debug peak, so it stays.
With a 40 MB limit a 2.5 MiB request aborts the child on a failed allocation (SIGABRT), and
with 30 MB or less the child fails earlier with a typed setup failure. Both are infrastructure
failures.

**Hostile templates.** "In process" is the same job run in a child without any limit, which is
what the worker would experience. Mechanisms were observed, not assumed.

| Template | In process | In the sandbox | Mechanism |
|---|---|---|---|
| `~` on a list doubled 40 times | still running after 25 s; virtual size 1,056 MiB and growing | about 2 s | CPU limit (SIGXCPU), permanent |
| `~` on a list doubled 20 times | 0.5 s | 0.3 to 0.5 s | typed budget from the output cap, permanent |
| `==` on two towers built with 28 doublings | still running after 25 s; 32 MiB, constant | about 2 s | CPU limit (SIGXCPU), permanent |
| a `* 1000` list repeated and formatted | 0.16 s | 0.07 to 0.5 s | typed budget from the formatter bound, permanent |
| 50,000 nested MJML wrappers | aborts the process in 50 ms | child aborts in 10 ms | stack overflow (SIGABRT), infrastructure |

The `~` template at 40 doublings is limited by CPU time, not by the address-space limit, so it
is a permanent failure, not an infrastructure one. It grows memory slowly (about 535 MiB
resident after 25 s), so a machine with a much faster CPU than the one measured could reach the
address-space limit first. That case is an abort, classified as infrastructure and retried up
to the worker's attempt limit.
