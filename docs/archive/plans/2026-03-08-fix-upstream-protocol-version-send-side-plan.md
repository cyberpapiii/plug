---
title: "fix: upstream MCP-Protocol-Version send-side"
type: fix
status: superseded
date: 2026-03-08
parent: docs/plans/2026-03-07-feat-mcp-spec-compliance-roadmap-plan.md
superseded_reason: >
  rmcp 1.1.0's StreamableHttpClientTransport already injects MCP-Protocol-Version
  automatically after initialization using the negotiated version from the server's
  InitializeResult. No plug code change needed. Confirmed by repo-local confidence
  test test_upstream_http_sends_protocol_version_header.
---

# fix: upstream MCP-Protocol-Version send-side

## Summary

Add explicit `MCP-Protocol-Version: 2025-11-25` on outgoing upstream HTTP requests so plug's
upstream Streamable HTTP client path matches the downstream validation posture already implemented
in PR #31.

This is a small, bounded protocol-correctness fix. It should be implemented and verified before any
larger Stream B work (elicitation/sampling, legacy SSE, OAuth).

## Current State

On `main`, downstream HTTP POST requests are validated for `MCP-Protocol-Version`, but upstream HTTP
requests are not explicitly configured to send that header.

The current upstream HTTP transport setup lives in:

- `plug-core/src/server/mod.rs`

The relevant flow today is:

1. `TransportType::Http` branch parses the upstream URL
2. builds `StreamableHttpClientTransportConfig::with_uri(url)`
3. conditionally adds bearer auth with `auth_header(...)`
4. creates the transport with `StreamableHttpClientTransport::from_config(...)`

## Implementation Changes

- In `plug-core/src/server/mod.rs`, extend the upstream HTTP transport configuration so every
  upstream Streamable HTTP request carries:
  - header name: `MCP-Protocol-Version`
  - header value: `2025-11-25`
- Preserve existing bearer auth behavior. The protocol-version header and auth header must both be
  present when auth is configured.
- Keep the change scoped to outgoing upstream HTTP requests only.
- Do not change downstream validation behavior.
- Do not broaden the change into legacy SSE, OAuth, or other transport work.

If the rmcp transport config already supports adding arbitrary headers directly, use that.
If not, implement the smallest adapter/wrapper that preserves the existing transport behavior while
adding the required header deterministically.

## Tests

Add targeted tests that prove the send-side header is configured on the upstream HTTP path.

Required scenarios:

1. HTTP upstream without auth
- build the HTTP transport path
- verify `MCP-Protocol-Version: 2025-11-25` is attached

2. HTTP upstream with auth token
- build the HTTP transport path
- verify both headers are attached:
  - `Authorization: Bearer ...`
  - `MCP-Protocol-Version: 2025-11-25`

3. Non-HTTP upstreams
- verify stdio path is unchanged

Prefer a focused unit/integration test around transport-config construction rather than a broad
end-to-end harness unless the existing code structure makes direct verification impossible.

## Acceptance Criteria

- outgoing upstream Streamable HTTP requests explicitly include `MCP-Protocol-Version: 2025-11-25`
- existing auth-header behavior still works
- stdio upstream transport is unchanged
- current downstream protocol-version validation remains unchanged
- `cargo test`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo fmt --check`

## Notes For The Implementing Team

- Treat `main` as the only source of truth.
- Do not re-audit roadmap status.
- This is a bounded implementation task, not a planning task.
- If the rmcp API makes explicit header injection impossible in the current transport path, stop and
  report the exact limitation instead of widening scope silently.
