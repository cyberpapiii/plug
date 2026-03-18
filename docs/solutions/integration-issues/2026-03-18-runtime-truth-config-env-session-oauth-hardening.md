---
title: Runtime truth, config env, session, and downstream OAuth hardening
date: 2026-03-18
category: integration-issues
status: completed
---

# Runtime truth, config env, session, and downstream OAuth hardening

## Problem

The remediation review surfaced four related reliability gaps:

- config loading depended on startup-time process env and the default config directory, which made `--config` and reload behavior drift from operator intent
- read-only operator commands auto-started the daemon, which could hide outage state instead of reporting it
- HTTP session cleanup and SSE backpressure handling left stale bridge state or dropped queued messages
- downstream OAuth persistence and auth-code exchange assumed the happy path, which made retries and multi-instance isolation brittle

## Solution

### Config and env resolution

- `load_config()` now resolves env references from process env plus the `.env` file adjacent to the active config path
- semantic validation now runs as part of `load_config()` instead of only through `plug config check`
- config writes are validated before being persisted
- dotenv parsing now preserves literal `#` characters unless they actually begin a comment

### Operator truth

- read-only views now inspect daemon availability without auto-starting it
- JSON responses were moved toward stable envelopes with explicit `runtime_available` / `status_source` metadata
- `auth status` now treats stored credentials and live runtime health as separate concepts instead of inventing degraded live state

### Session lifecycle

- timeout-expiry cleanup now unregisters downstream bridges as part of the same cleanup pass
- session admission prunes expired sessions before enforcing `max_sessions`
- when an SSE sender is full or closed, the message that triggered the failure is re-queued instead of being dropped

### Downstream OAuth

- persisted downstream OAuth state is now scoped by `public_base_url + oauth_client_id`
- auth-code exchange only consumes the code after redirect URI, expiry, and PKCE validation succeed

## Key decisions

- validation is enforced in the runtime path, not left as an optional operator command
- read-only commands favor truthfulness over convenience; explicit start/heal remains the job of `plug start`
- retryability was preferred for downstream auth codes because a mistyped verifier or redirect should not force a full re-auth flow

## Tests added

- config loads env vars from the `.env` file adjacent to the selected config path
- semantic config validation fails through `load_config()`
- expired sessions free capacity synchronously before new admission
- full SSE buffers re-queue targeted messages for later delivery
- invalid PKCE does not consume the downstream OAuth auth code
- downstream OAuth persistence is isolated per public base URL

## Follow-up

This tranche intentionally does not yet address:

- IPC response/notification interleaving during daemon proxy handshakes
- per-call reverse-request ownership and cancellation/progress correlation
- reload task-topology rebuilding and legacy SSE transport fixes

Those remain the next implementation waves from the remediation plan.
