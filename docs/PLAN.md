# Current Plan

This document tracks the current product state and the next remaining work after the merged Phase
1-3 tranches and Stream A follow-ups.

## Current State

`plug` has completed the major stabilization, protocol-surface, protocol-correctness, and roots
forwarding work:

- stabilization and truth fixes
- notification forwarding (logging, tools/list_changed, resources/list_changed, prompts/list_changed, `AuthStateChanged` observability via logging-channel fan-out)
- progress and cancellation routing
- resources/prompts forwarding with subscribe/unsubscribe lifecycle
- completion forwarding across all three transports (stdio, HTTP, IPC)
- structured output pass-through (outputSchema, structuredContent, resource_link)
- pagination
- capability synthesis (honest per-transport masking)
- meta-tool mode
- end-to-end transport coverage
- daemon continuity recovery (stdio clients via IPC proxy reconnect)
- session-store abstraction seam and stateless design prep
- MCP-Protocol-Version header validation on downstream HTTP POST requests
- MCP-Protocol-Version header on upstream HTTP requests (provided by rmcp 1.1.0 after initialization)
- subscription pruning and rebind on route refresh (todo 039 resolved)
- roots forwarding with union cache across stdio, HTTP, and daemon IPC
- elicitation + sampling reverse-request forwarding across stdio, HTTP, and daemon IPC (PR #34)
- legacy SSE upstream transport with HTTP→SSE auto-fallback, SSRF hardening, and auth support (PR #35)
- OAuth 2.1 + PKCE upstream auth with credential storage (keyring + file fallback), background token refresh, AuthRequired health state, CLI auth commands, doctor checks, and correct HTTP auth header construction (PR #36, PR #47)
- mock OAuth provider integration coverage for metadata discovery, auth-code exchange persistence with state cleanup, token refresh persistence, and reconnect using refreshed credentials (PR #51)
- daemon IPC notification parity: progress, cancelled, and list_changed push forwarding across IPC (PR #38)
- zero-downtime token refresh: actual OAuth refresh_token exchange before reconnect, with injected-token skip path, shared auth-failure classification for refresh/reconnect decisions, cache reload error propagation, reconnect retry without re-refreshing after transient failure, non-IPC `AuthStateChanged` observability via logging fan-out, and a distinct refresh-exchange observability signal (PR #42, PR #43, PR #44, PR #45, PR #50)

## What Exists Today

The current product shape is:

- `plug connect` for stdio downstream clients
- `plug serve` for Streamable HTTP downstream clients, with optional HTTPS via configured cert/key paths
- shared upstream routing through `Engine`, `ServerManager`, and `ToolRouter`
- daemon-backed local sharing with reconnecting IPC proxy sessions
- targeted notification fan-out to stdio, HTTP, and daemon IPC (resource subscribe still unsupported over IPC)
- meta-tool mode as an opt-in reduced discovery surface
- downstream HTTP bearer token auth for non-loopback binding

## Remaining Work

All major roadmap features are now implemented on `main`. The remaining work is smaller follow-up items:

### OAuth follow-up polish

- update the risk register to current remaining risks
- reduce the research breadcrumb list to the still-open questions
