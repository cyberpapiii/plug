---
title: "feat: Roots forwarding with union cache"
type: feat
status: completed
date: 2026-03-08
---

# feat: Roots forwarding with union cache

## Overview

Forward `roots/list` requests from upstream servers to downstream clients and propagate `roots/list_changed` notifications. When multiple downstream clients are connected (e.g., via daemon), present a union of all client roots to each upstream server.

This is Branch 2 from the extraction plan. Branch 1 (Stream A follow-ups) merged as PR #31. Roots is the next smallest Stream B item with no new crate dependencies.

## Problem Statement / Motivation

Upstream MCP servers (like Claude Code's server) send `roots/list` requests to discover the client's workspace roots (project directories, file system boundaries). plug currently has no handler for this — the request falls through to a default error. This means:

1. Upstream servers cannot discover workspace context through plug
2. `roots/list_changed` notifications from downstream clients are silently dropped
3. The `roots` capability is not advertised to upstream servers

## Proposed Solution

### Architecture

- **ToolRouter** gets a `client_roots: DashMap<NotificationTarget, Vec<Root>>` cache
- Each downstream transport registers roots on initialize and updates on `roots/list_changed`
- `list_roots_union()` deduplicates by URI across all connected clients
- `UpstreamClientHandler.list_roots()` returns the union cache
- Upstream servers receive `roots/list_changed` notification when any client's roots change

### Data Flow

```
Downstream client                    plug                         Upstream server
     |                                |                                |
     |-- initialize(roots cap) ------>|                                |
     |<-- initialized ----------------|                                |
     |<-- roots/list request ---------|                                |
     |-- roots/list response -------->|-- cache roots --------------->|
     |                                |-- roots/list_changed -------->|
     |                                |                                |
     |                                |<-- roots/list request --------|
     |                                |-- list_roots_union() -------->|
```

### Transport Coverage

| Transport | Register roots | Receive roots queries | Propagate list_changed |
|-----------|---------------|----------------------|----------------------|
| Stdio | `peer.list_roots()` on initialized + on `roots/list_changed` | Via `list_roots_union()` | Via `forward_roots_list_changed_to_upstreams()` |
| HTTP | `ServerRequest::ListRootsRequest` via SSE, response via POST | Via `list_roots_union()` | Via `forward_roots_list_changed_to_upstreams()` |
| Daemon IPC | `IpcRequest::UpdateRoots` round-trip | Via `list_roots_union()` | Via `forward_roots_list_changed_to_upstreams()` |

### Capability Advertisement

- Advertise `roots: { list_changed: true }` to upstream servers in `get_info()`
- Advertise `roots: { list_changed: true }` to downstream clients in `synthesized_capabilities()` only when at least one upstream advertises roots support
- For daemon IPC: mask `roots.list_changed` to `false` (IPC cannot push notifications)

## Technical Approach

### Phase 1: ToolRouter roots cache (plug-core/src/proxy/mod.rs)

- [x] Add `client_roots: DashMap<NotificationTarget, Vec<Root>>` field to `ToolRouter`
- [x] Add `set_roots_for_target(target, roots) -> bool` — returns true if roots changed
- [x] Add `clear_roots_for_target(target) -> bool` — returns true if entry existed
- [x] Add `list_roots_union() -> ListRootsResult` — deduplicates by URI across all clients
- [x] Add `forward_roots_list_changed_to_upstreams(&self)` — iterates healthy upstreams, calls `peer.notify_roots_list_changed()`

### Phase 2: Upstream handler (plug-core/src/server/mod.rs)

- [x] Implement `list_roots()` on `UpstreamClientHandler` — delegates to `router.list_roots_union()`
- [x] Advertise `RootsCapabilities { list_changed: Some(true) }` in `get_info()`

### Phase 3: Stdio transport (plug-core/src/proxy/mod.rs)

- [x] Add `downstream_peer: OnceLock<Peer<RoleServer>>` field to `ProxyHandler`
- [x] Store peer reference during initialization
- [x] On `on_initialized`: detect roots capability, spawn task to fetch roots via `peer.list_roots()`, cache via `set_roots_for_target()`
- [x] On `notifications/roots/list_changed`: re-fetch roots, update cache, forward to upstreams if changed
- [x] On shutdown/disconnect: `clear_roots_for_target()`, forward to upstreams if changed

### Phase 4: HTTP transport (plug-core/src/http/server.rs)

- [x] Add `roots_capable_sessions: DashMap<String, ()>` to `HttpState`
- [x] On initialize: detect roots capability, register session
- [x] Add `maybe_request_http_roots()` — sends `ServerRequest::ListRootsRequest` via SSE, awaits response via `pending_client_requests` oneshot
- [x] On `InitializedNotification`: call `maybe_request_http_roots()`
- [x] On `RootsListChangedNotification`: call `maybe_request_http_roots()`
- [x] On session delete: clear roots cache + forward if changed
- [x] Add `handle_client_response()` for `ClientResult::ListRootsResult` (and prepare for future elicitation/sampling results)

### Phase 5: Daemon IPC (plug/src/daemon.rs + plug/src/ipc_proxy.rs)

- [x] Add `UpdateRoots { session_id, roots }` variant to `IpcRequest`
- [x] Handle in `dispatch_request`: cache roots, forward to upstreams if changed
- [x] On deregister/disconnect: clear roots cache + forward if changed
- [x] In `IpcProxyHandler`: on `on_initialized` + `on_roots_list_changed`, fetch roots via `peer.list_roots()`, send `IpcRequest::UpdateRoots` to daemon
- [x] Detect roots capability from downstream peer during initialize

### Phase 6: Tests

- [x] Unit test: `list_roots_union()` deduplicates by URI
- [x] Unit test: `set_roots_for_target()` returns change detection correctly
- [x] Integration test: stdio client with roots -> upstream sees union
- [x] Integration test: HTTP client roots roundtrip via SSE
- [x] Integration test: roots change propagates `roots/list_changed` to upstream

## Reference Code

The `fix/subscription-rebind-confidence` branch has a working implementation. Key patterns to follow:

- `plug-core/src/server/mod.rs`: `list_roots()` handler pattern
- `plug-core/src/proxy/mod.rs`: `client_roots` DashMap, `list_roots_union()` dedup by URI
- `plug-core/src/http/server.rs`: `maybe_request_http_roots()` SSE reverse-request pattern
- `plug/src/daemon.rs`: `IpcRequest::UpdateRoots` dispatch
- `plug/src/ipc_proxy.rs`: `refresh_roots_via_daemon_shared()` IPC round-trip

## Acceptance Criteria

- [x] `roots/list` requests from upstream servers return the union of all downstream clients' roots
- [x] `roots/list_changed` from downstream clients propagates to all healthy upstream servers
- [x] Roots cached per-client, union computed on query
- [x] Client disconnect clears cached roots and notifies upstreams
- [x] All three transports (stdio, HTTP, IPC) support roots registration
- [x] `roots` capability advertised to upstream servers
- [x] Tests pass for all transport paths

## Dependencies

- PR #31 merged (Stream A follow-ups) — provides coalesced refresh infrastructure, notification patterns
- rmcp `ClientHandler::list_roots()` — confirmed exists in rmcp 1.0/1.1

## Sources & References

- Branch code: `fix/subscription-rebind-confidence` commit `8d3b062`
- Extraction plan: Branch 2 from roadmap audit assessment
- MCP spec: `roots/list` and `notifications/roots/list_changed`
- `docs/plans/2026-03-07-feat-mcp-spec-compliance-roadmap-plan.md`: Phase B3a
