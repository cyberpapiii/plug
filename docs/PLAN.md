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

## What Exists Today

The current product shape is:

- `plug connect` for stdio downstream clients
- `plug serve` for Streamable HTTP downstream clients, with optional HTTPS via configured cert/key paths
- shared upstream routing through `Engine`, `ServerManager`, and `ToolRouter`
- daemon-backed local sharing with reconnecting IPC proxy sessions
- targeted notification fan-out to stdio and HTTP (IPC limited to logging)
- meta-tool mode as an opt-in reduced discovery surface
- downstream HTTP bearer token auth for non-loopback binding

## Remaining Work

### Stream B: Connectivity Expansion (next priority)

These are the open features that require new infrastructure:

- **legacy SSE upstream transport** — custom transport via `reqwest-eventsource` for SSE-only remote servers (Neon, Firecrawl, Figma, Linear, Atlassian)
- **OAuth 2.1 + PKCE** — authenticate to upstream remote MCP servers with token refresh lifecycle

### Smaller open items

- daemon IPC notification parity beyond logging (progress, cancelled, list_changed push frames)
- dedicated tests for `structuredContent` and `resource_link` end-to-end pass-through

### Documentation and release hygiene

- update the risk register to current remaining risks
- reduce the research breadcrumb list to the still-open questions
