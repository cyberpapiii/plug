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
- full task lifecycle support for tool-backed tasks across stdio, HTTP, and daemon IPC
- upstream task pass-through when supported, with local wrapper-mode execution otherwise
- structured output pass-through (outputSchema, structuredContent, resource_link)
- pagination
- capability synthesis (honest per-transport masking)
- tool semantics enrichment (`readOnlyHint`, `destructiveHint`, `idempotentHint`, `openWorldHint`, `taskSupport`) and canonicalized branding/display metadata
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
- downstream OAuth remote server support and related config/runtime integration
- remote Claude connector follow-up fixes: protocol-version response adjustment, pagination cursor forwarding, larger page size, and connector stability improvements
- persisted token hydration before upstream connect
- downstream OAuth/operator hardening across discovery, metadata, challenge behavior, and non-interactive diagnostics
- topology-aware setup/link/repair/status flows that preserve and surface downstream transport/auth choices
- transport-aware live session inventory with explicit scope and availability across daemon proxy and downstream HTTP sessions
- daemon-owned downstream HTTP/HTTPS when the shared background service is running
- daemon-provided transport-complete live session truth in background-service mode
- pinned operator JSON contracts plus downstream HTTP inventory failure-path coverage
- performance and efficiency follow-through across auth-store reads, reload batching, SSE fanout, and config env traversal
- runtime-truth follow-up hardening across `status`, `tools`, `servers`, `clients`, and `doctor`
- explicit live reverse-request delivery failure handling for downstream HTTP sessions
- review-hardened task correctness around monotonic state transitions, reconnect-stable IPC ownership, and fail-closed pass-through dispatch

## What Exists Today

The current product shape is:

- `plug connect` for stdio downstream clients
- `plug serve --daemon` / `plug start` for the shared background runtime (IPC + HTTP/HTTPS)
- `plug serve` for explicit standalone foreground HTTP/HTTPS serving
- shared upstream routing through `Engine`, `ServerManager`, and `ToolRouter`
- daemon-backed local sharing with reconnecting IPC proxy sessions and daemon-owned downstream HTTP
- transport-aware operator inventory that can rely on daemon truth directly when the background service is running, while still falling back cleanly during non-daemon foreground HTTP serving
- targeted notification fan-out to stdio, HTTP, and daemon IPC (resource subscribe still unsupported over IPC)
- meta-tool mode as an opt-in reduced discovery surface
- downstream HTTP bearer token auth for non-loopback binding
- downstream OAuth mode for remote/server-card based authorization flows
- explicit runtime/auth/operator state vocabulary across `status`, `doctor`, `auth status`, `clients`, and `servers`
- single-flight reload application with bounded concurrent startup and safe shared registration
- pre-serialized broadcast SSE payloads for the hot HTTP notification path
- centralized env-reference traversal reused by config loading and doctor checks
- core MCP Tasks support as part of the routed downstream surface

## Remaining Work

All major roadmap features are now implemented on `main`.
No required roadmap work remains for the current production-ready bar.

Optional future scope only:

- fully live runtime reconfiguration, if the product bar is expanded beyond the current release scope
- continuing optional operator/runtime polish now that daemon mode owns the primary shared runtime
- further low-priority simplification of internal reload/session/SSE helper structure

## 2026-03-22 Tasks Tranche

On 2026-03-22, `main` absorbed the core Tasks tranche and immediate review fixes:

- task-wrapped `tools/call` plus `tasks/list`, `tasks/get`, `tasks/result`, and `tasks/cancel`
- daemon IPC, HTTP, and stdio parity for task flows
- upstream task pass-through proof via dedicated task-native integration coverage
- metadata enrichment and branding follow-through that landed alongside the tranche
- fixes for the blocking review findings raised during `ce:review`

## 2026-03-16 Reconciliation Note

The previously working off-main runtime line was reconciled into `main` on 2026-03-16, verified in
an isolated `main` worktree with a passing full test suite, and then pushed as the new canonical
baseline.

## 2026-03-17 Operator Truth Expansion

On 2026-03-17, `main` absorbed the follow-on operator/runtime hardening work that:

- aligned `status`, `doctor`, `auth status`, `clients`, and `servers` around one explicit auth/runtime vocabulary
- preserved downstream topology choices through setup/link/repair flows
- introduced transport-aware live-session inventory with explicit availability/scope semantics
- added regression coverage for JSON operator contracts and downstream HTTP inventory failure paths

## 2026-03-17 Daemon-Owned HTTP Runtime

On 2026-03-17, `main` also moved the shared background service to the next architecture step:

- daemon mode now owns downstream HTTP/HTTPS as well as IPC proxy sessions
- daemon `ListLiveSessions` can report transport-complete truth directly
- runtime/operator surfaces trust daemon-provided complete inventory when it is available
- standalone `plug serve` remains as an explicit foreground mode rather than the primary runtime authority

## 2026-03-18 Performance And Runtime Truth Hardening

On 2026-03-18, `main` absorbed a focused hardening pass that:

- unified OAuth credential snapshot reads and reduced duplicated auth-store IO
- added fail-fast handling for dead HTTP reverse-request targets, then tightened send-time live-delivery failure handling
- batched reload startup with configured concurrency while serializing reload execution and protecting shared upstream registration
- coalesced health-triggered refresh work and deduplicated proactive recovery scheduling
- pre-serialized SSE notification payloads for HTTP fanout
- centralized config env traversal and reused it in doctor env inspection
- clarified operator runtime truth when the daemon is reachable but IPC/runtime inspection is unavailable
