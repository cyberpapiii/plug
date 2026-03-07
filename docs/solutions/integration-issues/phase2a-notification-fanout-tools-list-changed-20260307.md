---
title: "Phase 2A tools/list_changed notification fan-out across upstream, stdio, and HTTP"
category: integration-issues
tags:
  - notifications
  - tools/list_changed
  - stdio
  - http
  - sse
  - rmcp
  - routing
  - correlation
  - review
module: plug-core
date: 2026-03-07
symptom: |
  `plug` could proxy tool calls correctly but silently dropped upstream server-initiated notifications. In practice, upstream `tools/list_changed` never refreshed the merged tool cache and never reached connected stdio or HTTP clients, leaving downstream tool visibility stale until a restart or reconnect.
root_cause: |
  Upstream clients were created as `RunningService<RoleClient, ()>`, so rmcp accepted notifications but discarded them through the default no-op `ClientHandler`. The runtime also had no dedicated protocol-notification bus, stdio retained no transport peer for later delivery, HTTP only stored SSE senders without an actual fan-out path, and the request-correlation layer ended at internal observability events rather than a reusable routing structure.
severity: high
related:
  - docs/brainstorms/2026-03-07-phase2a-notification-infrastructure-brainstorm.md
  - docs/plans/2026-03-07-feat-phase2a-notification-infrastructure-plan.md
  - docs/solutions/integration-issues/rmcp-sdk-integration-patterns-plug-20260303.md
  - docs/solutions/integration-issues/mcp-multiplexer-http-transport-phase2.md
  - docs/solutions/integration-issues/proxy-timeout-handling-semaphore-bounds-stdio-reconnect-20260306.md
  - plug-core/src/server/mod.rs
  - plug-core/src/proxy/mod.rs
  - plug-core/src/http/session.rs
  - plug-core/src/http/server.rs
  - plug-core/src/notifications.rs
---

# Phase 2A tools/list_changed notification fan-out across upstream, stdio, and HTTP

## Problem

Before Phase 2A, `plug` had a solid request/response path but no real protocol-notification path:

- upstream notifications were dropped by the `()` client handler
- upstream tool cache refresh only happened during startup/reconnect
- stdio clients had no retained peer for later server-initiated notifications
- HTTP sessions could register SSE senders but nothing actually delivered MCP notifications onto them

That meant `tools/list_changed` existed in capabilities but not in behavior.

## Investigation

The important code facts were:

- `ServerManager` used `RunningService<RoleClient, ()>` for upstreams
- rmcp only invokes notification hooks on real `ClientHandler` implementations
- `ToolRouter::refresh_tools()` rebuilt an atomic merged cache correctly, but nothing called it on upstream notifications
- `ProxyHandler` retained only `client_type`
- `SessionManager` tracked optional `sse_sender`, but the HTTP transport had no subscription to internal protocol events

The first implementation pass fixed the happy path, but review surfaced two important follow-ups:

1. HTTP fan-out could be blocked by one slow SSE client
2. repeated upstream `tools/list_changed` notifications could force redundant full-cache rebuilds inline on the notification callback path

## Solution

The final Phase 2A design added a dedicated protocol-notification path without overloading `EngineEvent`.

### 1. Replace the no-op upstream client handler

Each upstream now uses a real rmcp `ClientHandler`:

- `UpstreamClientHandler` owns the shared per-server tool snapshot
- `on_tool_list_changed()` refetches the notifying server’s full tool list through `context.peer.list_all_tools()`
- the handler updates the shared tool snapshot used by `ServerManager`

This changed upstream connections from “notifications are dropped” to “notifications mutate local server state.”

### 2. Add a dedicated internal protocol-notification bus

`ToolRouter` now owns a `broadcast::Sender<ProtocolNotification>`, separate from `EngineEvent`.

The only Phase 2A notification is:

```rust
pub enum ProtocolNotification {
    ToolListChanged,
}
```

This keeps observability and protocol delivery separate.

### 3. Coalesce global cache rebuilds behind notification refresh scheduling

The naive approach awaited `router.refresh_tools().await` inline on every upstream notification. Review flagged that as avoidable churn because each refresh walks all healthy upstreams and rebuilds all filtered caches.

The final branch instead schedules notification refreshes through `ToolRouter::schedule_tool_list_changed_refresh()`:

- mark refresh pending
- if no refresh worker is running, spawn one
- rebuild the merged cache
- emit a single downstream `ToolListChanged` notification
- if more notifications arrived during the refresh, loop once more

That keeps the notification callback fast and coalesces bursts of upstream changes.

### 4. Add stdio fan-out through the real connected peer

`ProxyHandler::initialize()` now:

- stores the detected client type as before
- captures the connected `Peer<RoleServer>`
- subscribes once to the protocol-notification bus

On `ProtocolNotification::ToolListChanged`, the stdio handler calls:

```rust
peer.notify_tool_list_changed().await
```

This gives stdio clients the same downstream MCP notification semantics as native servers.

### 5. Add HTTP fan-out as part of transport construction

The HTTP transport now starts its notification subscriber from `build_router()` itself rather than relying on an external imperative setup step.

`HttpState`:

- subscribes once to the protocol-notification bus
- serializes each protocol notification to a JSON-RPC message
- broadcasts it to active SSE sessions

This makes notification delivery part of the HTTP transport contract, not a caller convention.

### 6. Harden HTTP SSE fan-out against slow or expired sessions

The first pass used awaited `sender.send(...)` across all sessions, which allowed one full per-session channel to stall delivery to every later session.

The final branch changed `SessionManager::broadcast()` to:

- skip sessions whose timeout window has already expired
- remove expired sessions immediately
- use non-blocking `try_send(...)` to each SSE channel
- drop slow/full senders from fan-out rather than letting them create head-of-line blocking

That keeps global notification delivery available even if one HTTP client stops reading.

### 7. Add a minimal but real downstream correlation substrate

Phase 2A intentionally does not implement cancellation or progress passthrough yet, but it now preserves downstream request identity in a way later phases can build on:

- `DownstreamCallContext` stores transport, optional session ID, and downstream request ID
- active calls are stored by internal `call_id`
- a reverse lookup is also stored from downstream request identity back to active call ID
- entries are removed on success, failure, retry handoff, timeout, and teardown-sensitive paths

This is still minimal, but it is now more than a log record: it is a usable request-identity index for later routing work.

## Why It Works

The branch restores the missing notification invariants:

1. upstream server-initiated notifications are no longer silently dropped
2. a `tools/list_changed` notification updates the upstream-local tool snapshot and the merged router cache
3. the same notification reaches both downstream transport types
4. one bad HTTP SSE consumer cannot stall notification delivery to the rest
5. the notification path is protocol-specific, not mixed into observability state

## Verification

Focused feature tests added:

- `server::tests::upstream_tool_list_changed_refreshes_router_and_notifies_stdio_client`
  Covers the full chain:
  upstream notification -> shared upstream tool refresh -> merged router refresh -> stdio downstream notification

- `http::server::tests::tools_list_changed_reaches_http_sse_client`
  Covers HTTP SSE delivery of the JSON-RPC `notifications/tools/list_changed` message

- `http::session::tests::broadcast_prunes_expired_sessions_before_delivery`
  Verifies expired HTTP sessions are removed before fan-out

- `http::session::tests::broadcast_skips_full_senders_without_blocking_other_sessions`
  Verifies one full SSE channel does not block delivery to another session

Full validation commands run:

```bash
cargo check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```

## Related Prior Work

- [rmcp-sdk-integration-patterns-plug-20260303.md](./rmcp-sdk-integration-patterns-plug-20260303.md)
  Reinforced the need for atomic cache refreshes and real rmcp surface alignment.

- [mcp-multiplexer-http-transport-phase2.md](./mcp-multiplexer-http-transport-phase2.md)
  Established the HTTP/SSE transport boundary and stream invariants that Phase 2A needed to preserve.

- [proxy-timeout-handling-semaphore-bounds-stdio-reconnect-20260306.md](./proxy-timeout-handling-semaphore-bounds-stdio-reconnect-20260306.md)
  Important reminder that stdio transport repair must stay off the caller path and that transport hygiene matters after exceptional conditions.

## Prevention

- Do not advertise protocol notification capabilities without an end-to-end delivery path.
- For rmcp client roles, never use `()` when server-initiated notifications matter.
- Keep protocol notifications off the observability bus unless the payload and semantics are identical.
- Treat SSE fan-out as an availability surface: one slow receiver must not stall all others.
- When upstream notifications can burst, coalesce refresh work instead of rebuilding the entire merged cache per event.
- If a branch claims to add “correlation,” require both forward tracking and a reverse lookup from protocol-visible identity to active work.
