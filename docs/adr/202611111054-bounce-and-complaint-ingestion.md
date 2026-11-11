---
status: accepted
date: 2026-11-11
decision-makers: Jeremie Drouet
---

# Bounce and complaint ingestion over raw-message HTTP with a signed return path

## Context and Problem Statement

Catapulte sends mail but learns nothing about what happens afterwards. Remote servers
report failures as delivery status notifications (DSN, RFC 3464) sent to the envelope
sender, and mailbox providers report spam complaints as abuse feedback reports (ARF,
RFC 5965). To keep a suppression list accurate, Catapulte must receive these reports,
tie each one to an email it sent, and decide whether to suppress the recipient.

Anyone on the internet can send mail to a bounce address, so a report is untrusted
input. A forged report that suppresses a legitimate recipient is a denial of service
against that recipient's mail, and a naive design lets an attacker choose whom to
suppress.

## Decision Drivers

- Reports arrive by SMTP, but Catapulte is not a mail server and should not become one.
- The authentication of the channel must not be mistaken for the authentication of the
  report.
- A suppression must only follow from evidence an outsider cannot fabricate.
- No new column: an email is found from data Catapulte already stores.

## Decision

### Transport: raw message over HTTP

The operator's mail server (the MX for the bounce domain) forwards each received
message, unchanged, to `POST /inbound/feedback` as a `message/rfc822` body. The
endpoint is outside the API-key layer and requires a dedicated ingest secret
(`CATAPULTE_BOUNCE_INGEST_SECRET`, bearer, constant-time compare). Catapulte parses the
message; the MX needs no knowledge of the report formats.

### Trust boundary

- The ingest secret authenticates **the forwarder only**. It says the message came
  through the operator's pipeline, not that its contents are true.
- Report contents are untrusted input. Every field is validated, and the recipient named
  in a report must be one of the recipients of the matched email.
- A **signed VERP address is a bearer capability**. The return path is
  `bounces+{uuid}-{tag}@{bounce domain}`, where the tag is the first 10 bytes of
  HMAC-SHA256 (keyed with `CATAPULTE_BOUNCE_SECRET`) over the email id, in lowercase
  base32. Holding a valid address proves the report was written by someone who was given
  that address, that is, a server that handled the message, or someone who read it.
- **Message-ID correlation is never authentication.** The `Message-ID` of every message
  embeds the email id, and the id is visible to every recipient, so it can be quoted by
  anyone.

### Which reports may suppress

The signed tag binds the **email**, not a recipient. Every recipient of a message sees the
return path (in `Return-Path` or the bounce headers of their own copy), so any recipient can
build a report carrying a valid tag. Automatic suppression is therefore limited to emails with
**exactly one recipient** across To, Cc and Bcc (the final envelope). A report for a
multi-recipient email only ever produces an event.

| Report | Correlation | Result |
|--------|-------------|--------|
| DSN, hard bounce, single-recipient email | valid VERP tag, existing email, recipient is its one recipient | `bounced` event and suppression |
| DSN, hard bounce, multi-recipient email | same, recipient is one of them | `bounced` event only |
| DSN, soft bounce | same | `bounced` event only |
| DSN | no VERP address | rejected (422) |
| ARF complaint, single-recipient email | valid VERP tag, existing email, recipient matches | `complained` event and suppression |
| ARF complaint, multi-recipient email | valid VERP tag, recipient is one of them | `complained` event only |
| ARF complaint | `Message-ID` only, existing email, recipient matches | `complained` event with `verified: false`, never a suppression |
| any | VERP tag present but malformed or failing the HMAC check | rejected (422), no fallback to `Message-ID`, no event |
| any | unknown email, or recipient not on the email, or unsupported report type | rejected (422) |

Rejected reports are not stored and are logged without their body. `bounced` and
`complained` are informational events: they never change the email's status.

### Spoofing residue

Signing narrows the attack, it does not remove it. A party that knows a signed VERP
address can forge a report for **that one email, naming any of its recipients**. It cannot
name an address that was not a recipient of that email, and it cannot forge an address for an
email it never saw.

- On a single-recipient email the only possible target is the recipient itself, who can
  already stop mail to itself, so a forged report gives no new power. The reverse case, a
  third party who obtains the address (for example from a forwarded message), can suppress
  that one recipient; this is accepted.
- On a multi-recipient email a co-recipient could name another recipient in a forged hard
  bounce. This is why the decision below limits suppression to single-recipient emails.
  Events for multi-recipient emails can still be forged, and consumers must treat them as
  advisory.

### Return path secret

`CATAPULTE_BOUNCE_SECRET` must be random and at least 32 bytes (for example
`openssl rand -base64 48`); shorter secrets are refused at startup.

## Considered Options (suppression of multi-recipient emails)

1. **Suppress only when the email had exactly one recipient (chosen).** Closes the
   cross-recipient forgery, and keeps automatic suppression for single-recipient
   transactional mail, which is the common case.
2. **A separate VERP address per recipient, one SMTP transaction each.** Would bind the tag
   to the recipient. Rejected: it changes the delivery model (one transaction per recipient,
   Cc and Bcc semantics, more load on the relay).
3. **Accept the risk and suppress on any valid tag.** Rejected: a co-recipient could
   suppress another recipient.
4. **Events only, never suppress.** Rejected: it loses the main value of the feature for
   single-recipient mail, where nothing can be forged against a third party.

### Operator guidance

Restrict what the MX forwards to Catapulte: only mail addressed to the bounce domain, and
preferably only messages that look like DSN or ARF reports. Keep the ingest secret and the
bounce secret apart from each other, and rotate the bounce secret knowing that addresses
already sent stop verifying.

## Consequences

- Hard bounces and verified complaints on single-recipient emails suppress without an
  operator in the loop; multi-recipient emails need an operator or a later per-recipient
  design to suppress.
- Complaints reported without the VERP address stay visible as events but never act.
- The recipient list used for the single-recipient rule is the one stored at submission. The
  worker may later drop newly suppressed recipients, so the stored list can only be a
  superset of what was sent: the error is on the safe side and can only skip a suppression,
  never cause a wrong one.
- Rotating the bounce secret makes reports for in-flight mail unverifiable; they are
  rejected, not suppressed.
- The bounce domain needs its own MX and SPF; see the readme.
