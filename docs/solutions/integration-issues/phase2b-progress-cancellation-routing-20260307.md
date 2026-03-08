---
title: "Phase 2B progress and cancellation routing across stdio and HTTP"
category: integration-issues
tags:
  - rmcp
  - progress
  - cancellation
  - routing
  - stdio
  - sse
  - transport-parity
  - correlation
module: plug-core
date: 2026-03-07
note: "Historical solution record for the original Phase 2B landing. Current transport parity must be checked against main."
symptom: |
  After Phase 2A, plug could receive upstream notifications and broadcast tools/list_changed,
  but request-scoped control flow was still missing. Downstream notifications/cancelled were
  accepted and dropped, upstream progress notifications had no routing path, and the initial
  implementation had two follow-up gaps: progress-token routing could race request registration,
  and targeted HTTP notifications vanished when a session existed without an attached SSE stream.
root_cause: |
  Tool calls were still issued through Peer<RoleClient>::call_tool(), which hides the upstream
  request ID needed for exact cancellation forwarding. The active-call layer only preserved
  downstream identity, not upstream request or progress-token lookups. The HTTP session manager
  tracked only the active SSE sender, so targeted notifications had no buffering path when a
  session was valid but temporarily streamless.
severity: high
related:
  - docs/brainstorms/2026-03-07-phase2b-progress-cancellation-routing-brainstorm.md
  - docs/plans/2026-03-07-feat-phase2b-progress-cancellation-routing-plan.md
  - docs/solutions/integration-issues/phase2a-notification-infrastructure-tools-list-changed-20260307.md
  - docs/solutions/integration-issues/rmcp-sdk-integration-patterns-plug-20260303.md
  - plug-core/src/proxy/mod.rs
  - plug-core/src/server/mod.rs
  - plug-core/src/http/server.rs
  - plug-core/src/http/session.rs
  - plug-core/src/notifications.rs
---

# Phase 2B progress and cancellation routing across stdio and HTTP

## Problem

Phase 2A solved global notification plumbing, but it did not yet solve request-scoped control flow.

That left three user-visible gaps:

1. downstream `notifications/cancelled` were validated and dropped
2. upstream `notifications/progress` could be received by rmcp but were not routed to the correct caller
3. the active-call layer preserved only downstream identity, not enough upstream correlation to route cancellation or progress end to end

Code review also surfaced two follow-up reliability gaps in the first Phase 2B pass:

1. upstream progress notifications could arrive before the progress-token lookup existed
2. targeted HTTP progress/cancelled notifications were silently lost when a session was valid but no SSE stream was attached

## Investigation

The key API fact from `rmcp 1.0.0` was that `Peer<RoleClient>::call_tool(...)` is too high-level for cancellation routing. It returns only the final `CallToolResult` and hides the upstream request ID.

The smallest viable path was to switch to `send_cancellable_request(...)`, which returns a `RequestHandle` containing:

- the generated upstream request ID
- the peer needed to cancel the request
- the progress token attached to the request

That let `plug` keep using rmcp’s typed request path without dropping to raw transport handling.

## Solution

### 1. Replace the upstream convenience call with a cancellable request handle

`ToolRouter::call_tool_inner()` now builds a typed upstream `ClientRequest::CallToolRequest(...)` and sends it with `send_cancellable_request(...)`.

That exposes the upstream request ID and allows `plug` to:

- map downstream request identity to the exact upstream request
- preserve downstream progress tokens on the upstream request
- rely on `RequestHandle::await_response()` for timeout semantics

### 2. Expand the active-call record

The active-call layer now tracks both sides of the request:

- downstream transport identity
- downstream request ID
- downstream client/session identity
- upstream server ID
- upstream request ID
- optional progress token

The router keeps:

- downstream lookup for cancellation
- upstream request lookup for upstream `notifications/cancelled`
- upstream progress-token lookup for upstream `notifications/progress`

### 3. Route downstream cancellation upstream

Both downstream entry points now forward cancellation instead of dropping it:

- stdio via `ProxyHandler::on_cancelled(...)`
- HTTP via `post_mcp(...)` notification handling

Those paths resolve the active call and send `notify_cancelled(...)` to the correct upstream peer using the captured upstream request ID.

### 4. Route upstream progress and upstream cancellation back downstream

`UpstreamClientHandler` now handles:

- `on_progress(...)`
- `on_cancelled(...)`

and routes both through the protocol-notification layer as targeted notifications:

- stdio targets are keyed by the connected proxy client ID
- HTTP targets are keyed by session ID

This keeps list-change notifications on the global bus while request-scoped progress/cancelled messages are targeted to the correct downstream only.

### 5. Fix the progress-token registration race

The initial implementation created the upstream request handle before inserting the progress lookup. That left a gap where an upstream server could emit progress immediately and beat the map registration.

The final fix pre-registers the active call with downstream and progress-token identity before the request is sent, then attaches the upstream request ID to the record once the request handle is available.

### 6. Queue targeted HTTP notifications when no SSE stream is attached

`SessionManager` now keeps a bounded pending-notification queue per session. Targeted HTTP progress/cancelled messages are:

- delivered immediately if an SSE sender is active
- queued if the session is valid but no stream is attached
- flushed on the next `set_sse_sender(...)`

This closes the reliability gap where targeted notifications disappeared silently for valid HTTP sessions without an active stream.

## Verification

Focused tests added:

- `server::tests::stdio_progress_and_cancellation_route_end_to_end`
  Verifies stdio targeted progress delivery and real downstream cancellation forwarding to an upstream server.

- `http::server::tests::targeted_progress_reaches_http_sse_session`
  Verifies targeted HTTP progress delivery reaches the correct SSE session.

- `proxy::tests::route_upstream_progress_publishes_targeted_notification`
  Verifies the real upstream progress-token lookup path produces the correct targeted notification.

Full verification passed:

```bash
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```

## Prevention

- Do not use the `call_tool(...)` convenience wrapper when you need the upstream request ID.
- Preserve downstream `progressToken` explicitly when proxying tool calls.
- Register progress-token routing before the upstream request can emit progress.
- Treat global notifications and request-scoped notifications as different delivery classes.
- For HTTP, do not assume “valid session” implies “active SSE stream”; queue targeted notifications when needed.
- Keep active-call cleanup symmetric across success, timeout, transport error, reconnect retry, and downstream cancellation.
