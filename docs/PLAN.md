# Current Plan

This document tracks the current product state and the next remaining work after the merged Phase
1-3 tranches and Stream A follow-ups.

## Current State

`plug` has completed the major stabilization, protocol-surface, protocol-correctness, and roots
forwarding work:

- stabilization and truth fixes
- notification forwarding (logging, tools/list_changed, resources/list_changed, prompts/list_changed)
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
- OAuth 2.1 + PKCE upstream auth with credential storage (keyring + file fallback), background token refresh, AuthRequired health state, CLI auth commands, and doctor checks (PR #36)
- daemon IPC notification parity: progress, cancelled, and list_changed push forwarding across IPC (PR #38)

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

- `plug auth complete` command for non-interactive code exchange (agent-native)
- localhost callback listener for `plug auth login` (currently uses manual code entry)
- IPC auth commands (`AuthStatus`, `InjectToken`, `AuthStateChanged` push notification)
- zero-downtime reconnect on token refresh (pre-create transport before swap)
- mock OAuth provider integration tests

### Documentation and release hygiene

- update the risk register to current remaining risks
- reduce the research breadcrumb list to the still-open questions
