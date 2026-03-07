---
title: "Phase 2A notification infrastructure: upstream tools/list_changed fan-out"
category: integration-issues
tags:
  - rmcp
  - notifications
  - tools-list-changed
  - sse
  - stdio
  - transport-parity
  - routing
  - correlation
  - code-review
module: plug-core
date: 2026-03-07
symptom: |
  plug could proxy tool calls correctly but it silently dropped upstream server-initiated
  notifications. In practice, tools/list_changed never reached downstream stdio or HTTP
  clients, HTTP SSE streams registered successfully but emitted no MCP notifications, and
  there was no dedicated internal protocol-notification path to build later progress or
  cancellation work on top of.
root_cause: |
  Upstream MCP clients were instantiated with rmcp's no-op ClientHandler (`()`), so
  server-initiated notifications were discarded. ToolRouter owned merged tool state but
  had no protocol-notification bus, ProxyHandler retained no downstream peer for later
  notification emission, HTTP SessionManager stored SSE senders without a delivery path,
  and the first implementation pass needed review follow-ups to prevent slow SSE clients
  from stalling global fan-out and clustered tools/list_changed events from forcing
  redundant full-cache rebuilds.
severity: high
related:
  - docs/brainstorms/2026-03-07-phase2a-notification-infrastructure-brainstorm.md
  - docs/plans/2026-03-07-feat-phase2a-notification-infrastructure-plan.md
  - docs/solutions/integration-issues/rmcp-sdk-integration-patterns-plug-20260303.md
  - docs/solutions/integration-issues/mcp-multiplexer-http-transport-phase2.md
  - docs/solutions/integration-issues/proxy-timeout-handling-semaphore-bounds-stdio-reconnect-20260306.md
  - plug-core/src/server/mod.rs
  - plug-core/src/proxy/mod.rs
  - plug-core/src/http/server.rs
  - plug-core/src/http/session.rs
  - plug-core/src/notifications.rs
  - plug/src/runtime.rs
---

# Phase 2A notification infrastructure: upstream tools/list_changed fan-out

## Problem

`plug` had a working tool-call path but no end-to-end server-notification path.

That showed up in four concrete ways:

1. upstream `tools/list_changed` notifications were accepted by `rmcp` and then discarded
2. downstream stdio clients never received `notifications/tools/list_changed`
3. downstream HTTP SSE clients could open streams but still never receive MCP notifications
4. the codebase had no protocol-notification substrate to attach later progress/cancellation work to

The first implementation pass fixed the missing path, but review surfaced two more real risks:

1. one slow SSE client could stall fan-out for every HTTP session
2. clustered upstream list-change notifications could force repeated full merged-cache rebuilds

## Investigation

The useful repository facts were:

- upstream clients in [`plug-core/src/server/mod.rs`](../../../plug-core/src/server/mod.rs) used `RunningService<RoleClient, ()>`
- rmcp's default `impl ClientHandler for ()` drops upstream notifications
- [`ToolRouter`](../../../plug-core/src/proxy/mod.rs) owned the merged tool cache but had no dedicated protocol-notification channel
- [`ProxyHandler`](../../../plug-core/src/proxy/mod.rs) only stored `client_type`, so there was no stdio peer handle for later emission
- [`SessionManager`](../../../plug-core/src/http/session.rs) stored `sse_sender` values but had no broadcast path that serialized JSON-RPC notifications and delivered them
- the initial review also showed that serialized SSE delivery and inline full-cache refresh work would become the next reliability bottlenecks if left alone

That established the right shape for the fix:

- replace the no-op upstream handler
- keep protocol notifications separate from `EngineEvent`
- share one transport-neutral notification bus through `ToolRouter`
- retain enough downstream identity to emit notifications now and extend toward progress/cancellation later

## Solution

### 1. Replace the no-op upstream client handler

Upstream connections now use a real `ClientHandler`:

- `RunningService<RoleClient, ()>`
- becomes `RunningService<RoleClient, Arc<UpstreamClientHandler>>`

[`UpstreamClientHandler`](../../../plug-core/src/server/mod.rs) handles `on_tool_list_changed(...)` by:

- calling `context.peer.list_all_tools()`
- updating the shared per-server tool snapshot
- triggering a coalesced router refresh

### 2. Make upstream tool snapshots shared and mutable

`UpstreamServer.tools` changed from a plain `Vec<Tool>` to shared mutable state:

- `Arc<ArcSwap<Vec<Tool>>>`

That matters because notification callbacks cannot replace the whole `UpstreamServer`, and `RunningService` ownership makes whole-object replacement the wrong shape for notification-driven updates.

`ServerManager::get_tools()` and status reporting now read the live shared snapshot instead of assuming a static vector captured at startup.

### 3. Add a dedicated protocol-notification bus

[`plug-core/src/notifications.rs`](../../../plug-core/src/notifications.rs) introduced `ProtocolNotification`, and [`ToolRouter`](../../../plug-core/src/proxy/mod.rs) now owns a `broadcast::Sender<ProtocolNotification>`.

This keeps the separation clean:

- `EngineEvent` remains observability-only
- wire-level MCP notifications use a dedicated internal path

### 4. Coalesce notification-driven cache refreshes

The first version refreshed the merged tool cache inline on every upstream `tools/list_changed`.

The final version moves that work behind a coalescing scheduler in [`ToolRouter`](../../../plug-core/src/proxy/mod.rs):

- clustered notifications set a pending flag
- one background refresh rebuilds the merged cache
- if another notification arrived during refresh, one more pass runs
- downstream notification fan-out happens after the refreshed cache is in place

This avoids redoing a full merged-cache rebuild once per event under bursty upstream churn.

### 5. Wire stdio fan-out

[`ProxyHandler`](../../../plug-core/src/proxy/mod.rs) now:

- subscribes to the router’s protocol-notification bus after initialize
- retains the downstream `Peer<RoleServer>`
- forwards `ProtocolNotification::ToolListChanged` through `peer.notify_tool_list_changed().await`

That makes stdio parity real instead of leaving notification delivery HTTP-only.

### 6. Wire HTTP SSE fan-out into the transport contract

[`build_router(...)`](../../../plug-core/src/http/server.rs) now ensures notification fan-out is started as part of HTTP transport construction. It is no longer a separate runtime convention that callers have to remember.

[`SessionManager::broadcast(...)`](../../../plug-core/src/http/session.rs) was also hardened:

- expired sessions are pruned before delivery
- full or closed per-session channels are treated as stale and cleared
- delivery no longer awaits one sender after another, so a slow SSE client cannot stall every other HTTP session

### 7. Add a minimal correlation substrate

[`DownstreamCallContext`](../../../plug-core/src/proxy/mod.rs) now preserves:

- downstream transport
- optional session ID
- protocol-visible request ID

The router tracks active calls and a reverse lookup keyed by downstream request identity. That is intentionally narrow, but it is a real routing substrate rather than a write-only audit record.

It still does **not** implement progress/cancellation behavior yet. It only preserves the identity needed for that later tranche.

## Verification

Focused tests added:

- `server::tests::upstream_tool_list_changed_refreshes_router_and_notifies_stdio_client`
  Verifies a real upstream `tools/list_changed` updates upstream tools, refreshes the router, notifies a downstream stdio client, and leaves no leaked active-call state after a tool call.

- `http::server::tests::tools_list_changed_reaches_http_sse_client`
  Verifies an HTTP SSE client receives serialized `notifications/tools/list_changed`.

- `http::session::tests::broadcast_prunes_expired_sessions_before_delivery`
  Verifies expired HTTP sessions do not keep receiving notifications.

- `http::session::tests::broadcast_skips_full_senders_without_blocking_other_sessions`
  Verifies one slow/full SSE client does not block fan-out to another.

Full verification passed:

```bash
cargo check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```

## Prevention

- Never use rmcp’s no-op `ClientHandler` for upstream clients if server-initiated messages matter.
- Keep protocol notifications separate from observability events.
- Treat `tools/list_changed` refresh as atomic state maintenance: per-server snapshot and merged router cache must stay coherent.
- Coalesce notification-driven refresh work instead of rebuilding once per event.
- Make transport correctness part of transport construction, not a side-call convention.
- Never let one slow SSE client stall global notification delivery.
- Re-check session liveness when delivering notifications, not only when handling requests.
- Preserve downstream request identity early so later progress/cancellation work has something real to attach to.
