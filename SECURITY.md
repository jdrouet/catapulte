# Security Policy

## Supported versions

catapulte ships a single version for the whole workspace. Security fixes are
applied only to the latest release.

| Version                          | Supported          |
| -------------------------------- | ------------------ |
| Latest `2.x` release             | :white_check_mark: |
| `< 2.0` (pre-rewrite `0.x` / `1.0.0` alphas) | :x:        |

## Reporting a vulnerability

**Please do not open a public issue for security problems.**

Report privately through GitHub: go to the repository's **Security** tab and
choose **"Report a vulnerability"** (GitHub Private Vulnerability Reporting).
This keeps the report confidential until a fix is available.

In your report, please include:

- the affected version (or commit),
- a description of the issue and its impact,
- steps to reproduce, and a proof of concept if you have one,
- any suggested remediation.

What to expect:

- We aim to acknowledge a report within a few days.
- We practice **coordinated disclosure**: we work on a fix privately, release
  it, and only then publish the advisory. We're happy to credit you unless you
  prefer to stay anonymous.

## Scope

catapulte is a self-hosted service that accepts email-submission requests and
delivers them over SMTP. The following are **in scope**:

- **Server-side request forgery (SSRF)** in the remote-fetch paths: URL-based
  attachments and remote MJML templates (`mj-include` fetched over HTTP).
- **Authentication / authorization** flaws on the HTTP API (the
  `CATAPULTE_HTTP_API_KEY` bearer check), including bypasses.
- **Injection**: email header/recipient injection, or template injection via
  MJML plus caller-supplied variables.
- **Credential or data exposure**: leaking SMTP sender credentials, storage or
  queue connection strings, or another consumer's email/event data — including
  via logs, lifecycle events, or error messages.
- Memory-safety or correctness bugs in catapulte's own crates that are
  remotely triggerable.

The following are **out of scope**:

- **Operator misconfiguration** — most importantly, exposing the API with
  `CATAPULTE_HTTP_API_KEY` unset on an untrusted network. Unauthenticated mode
  is documented as trusted-network-only.
- **Volumetric denial of service / abuse.** catapulte has no built-in rate
  limiting or abuse protection by design; operators are expected to run it
  behind a gateway or reverse proxy that enforces limits.
- Features explicitly listed as out of scope in the [readme](./readme.md)
  (bounce ingestion, scheduled sends, suppression lists, multi-tenant auth).
- Vulnerabilities already tracked by the repository's dependency scanning, or
  in dependencies that are not reachable from the shipped binary (for example,
  test-only tooling).
- Findings that require physical access, social engineering, or
  already-compromised infrastructure.

## Hardening for operators

To run catapulte safely:

- Set `CATAPULTE_HTTP_API_KEY` and run behind a reverse proxy; do not expose the
  service directly to the public internet.
- Enforce request rate limits at that proxy/gateway.
- Keep SMTP credentials and connection strings in a secret store, not in plain
  shell history or committed files.
- Restrict which hosts remote attachments and templates may be fetched from.

## Safe harbor

We will not pursue or support legal action against anyone who reports a
vulnerability in good faith, makes a reasonable effort to avoid privacy
violations and service disruption, and does not access or modify data beyond
what is necessary to demonstrate the issue.
