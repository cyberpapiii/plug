---
title: "Resource Subscribe/Unsubscribe Forwarding with Subscription Lifecycle Management"
category: integration-issues
tags: [resources, subscriptions, notifications, lifecycle, cleanup, mcp, proxy, broadcast]
module: plug-core
symptom: "Missing resources/subscribe forwarding; subscription state leaks on client disconnect; HTTP transport advertises subscribe capability but returns METHOD_NOT_FOUND"
root_cause: "Subscription registry implemented without disconnect cleanup paths; HTTP request dispatcher missing subscribe/unsubscribe match arms"
date: 2026-03-07
pr: "#30"
severity: high
---

# Resource Subscribe/Unsubscribe Forwarding with Subscription Lifecycle Management

## Problem

The MCP multiplexer forwarded resource reads and listings but had no `resources/subscribe` or `resources/unsubscribe` support. Downstream clients could not receive `notifications/resources/updated` from upstream servers. The capability synthesis hardcoded `subscribe: None`.

## Investigation

### Initial Implementation

The first implementation (commit `1194328`) added:
- Subscription registry (`DashMap<String, HashSet<NotificationTarget>>`) on `ToolRouter`
- First-subscriber/last-subscriber optimization for upstream subscribe/unsubscribe
- `on_resource_updated()` handler on `UpstreamClientHandler`
- `ResourceUpdated` variant in `ProtocolNotification` enum
- Targeted fan-out in both stdio and HTTP notification loops
- IPC capability masking (daemon clients can't receive push notifications)
- Truthful capability synthesis

### Review Findings (6-agent parallel review)

The review uncovered three critical gaps:

**1. HTTP subscribe handler missing (P1)**

The HTTP `handle_request()` function had no `ClientRequest::SubscribeRequest` or `ClientRequest::UnsubscribeRequest` match arms. HTTP clients received `METHOD_NOT_FOUND` despite the server advertising `resources.subscribe: true`. The notification delivery side (SSE fan-out) was correctly wired, but no HTTP client could ever trigger it.

**2. Subscription leak on client disconnect (P1)**

When a stdio or HTTP client disconnected without calling `resources/unsubscribe`, the `resource_subscriptions` DashMap entries remained orphaned. This caused:
- Unbounded memory growth over client connect/disconnect cycles
- Upstream subscriptions never cleaned up (subscriber count never reaches zero)
- Broadcast channel pollution with notifications routed to dead targets

**3. No rollback on upstream subscribe failure (P2)**

In `subscribe_resource()`, the local subscriber was inserted *before* the upstream subscribe call. If the upstream call failed, the local entry persisted with a subscriber that would never receive updates. Subsequent subscribers saw `is_first == false` and skipped the upstream call entirely.

## Solution

### Fix 1: HTTP subscribe/unsubscribe handlers

Added `ClientRequest::SubscribeRequest` and `ClientRequest::UnsubscribeRequest` arms to `handle_request()` in `plug-core/src/http/server.rs`, following the `ReadResourceRequest` pattern:

```rust
ClientRequest::SubscribeRequest(sub_req) => {
    let session_id = extract_session_id(headers)?;
    validate_session_header(headers, state.sessions.as_ref())?;
    let target = NotificationTarget::Http {
        session_id: Arc::from(session_id.as_str()),
    };
    match state.router.subscribe_resource(&sub_req.params.uri, target).await {
        Ok(()) => {
            let response_msg = ServerJsonRpcMessage::response(
                ServerResult::EmptyResult(().into()), request_id,
            );
            json_response(&response_msg)
        }
        Err(mcp_err) => {
            let response_msg = ServerJsonRpcMessage::error(mcp_err, request_id);
            json_response(&response_msg)
        }
    }
}
```

Note: `EmptyResult` requires `().into()` (not `()`) because rmcp uses `EmptyObject`, not unit.

### Fix 2: Subscription cleanup on disconnect

Added `cleanup_subscriptions_for_target()` to `ToolRouter`:

```rust
pub async fn cleanup_subscriptions_for_target(&self, target: &NotificationTarget) {
    let mut uris_to_unsubscribe: Vec<(String, String)> = Vec::new();

    self.resource_subscriptions.retain(|uri, subscribers| {
        subscribers.remove(target);
        if subscribers.is_empty() {
            let snapshot = self.cache.load();
            if let Some(server_id) = snapshot.resource_routes.get(uri).cloned() {
                uris_to_unsubscribe.push((uri.clone(), server_id));
            }
            false // remove the empty entry
        } else {
            true
        }
    });

    for (uri, server_id) in uris_to_unsubscribe {
        if let Some(upstream) = self.server_manager.get_upstream(&server_id) {
            if let Err(error) = upstream.client.peer().unsubscribe(...).await {
                tracing::warn!(uri = %uri, error = %error,
                    "failed to unsubscribe upstream during target cleanup");
            }
        }
    }
}
```

Called from:
- **Stdio**: After the notification fan-out loop breaks (peer send failure or channel close)
- **HTTP DELETE**: After `sessions.remove()` in `delete_mcp`

Key pattern: Use `DashMap::retain()` to atomically iterate and remove in one pass, collecting URIs that need upstream unsubscribe. Then send upstream unsubscribes outside the DashMap lock.

### Fix 3: Rollback on upstream subscribe failure

```rust
if is_first {
    if let Err(error) = upstream.client.peer().subscribe(...).await {
        // Roll back the local subscription
        if let Some(mut entry) = self.resource_subscriptions.get_mut(uri) {
            entry.remove(&target);
            if entry.is_empty() {
                drop(entry);
                self.resource_subscriptions.remove(uri);
            }
        }
        return Err(match error { ... });
    }
}
```

## Key Patterns and Lessons

### 1. Every new DashMap registry needs a cleanup path

When adding stateful registries (subscriptions, active calls, correlation maps), immediately ask: "What cleans this up when the client goes away?" If the answer is "the client calls unsubscribe," that's insufficient -- clients crash, connections drop, sessions time out.

**Checklist for new registries:**
- [ ] Explicit cleanup method exists
- [ ] Called from stdio disconnect path
- [ ] Called from HTTP session DELETE
- [ ] Called from HTTP session timeout reaper (if possible)
- [ ] Called from daemon IPC deregister path (if applicable)

### 2. HTTP request dispatcher must match all new ClientRequest variants

The HTTP `handle_request()` uses manual `ClientRequest` matching (not rmcp's trait dispatch). Every new MCP method that gets ServerHandler trait support must ALSO get an explicit match arm in `handle_request()`. The `_ =>` catch-all silently returns METHOD_NOT_FOUND.

**Checklist when adding new MCP method support:**
- [ ] `ServerHandler` trait override in `ProxyHandler` (stdio path)
- [ ] `ClientRequest::*` match arm in HTTP `handle_request()`
- [ ] IPC dispatch arm in `daemon.rs` (or explicit error if unsupported)
- [ ] Capability synthesis reflects the new capability truthfully

### 3. Insert-then-call patterns need rollback

When inserting into a registry before making an async call that can fail, always roll back the insertion on failure. Otherwise the registry enters an inconsistent state that subsequent operations build on incorrectly.

### 4. `UnsubscribeRequestParams` is `#[non_exhaustive]` without `::new()`

Unlike `SubscribeRequestParams`, rmcp's `UnsubscribeRequestParams` has no `new()` constructor. Use `serde_json::from_value` as a workaround:

```rust
serde_json::from_value::<UnsubscribeRequestParams>(
    serde_json::json!({ "uri": uri }),
).expect("UnsubscribeRequestParams from known-good JSON")
```

### 5. Notification fan-out follows a consistent pattern

Resource subscriptions follow the exact same pattern as Progress and Cancelled:
1. `ProtocolNotification` enum variant with `target` + `params`
2. Upstream `ClientHandler` callback creates the notification
3. `publish_protocol_notification()` sends to broadcast channel
4. Stdio loop filters by `client_id` match
5. HTTP loop filters by `session_id` match and calls `send_to_session`

## Prevention

1. **Review checklist**: When adding any new MCP method forwarding, check all three transports (stdio, HTTP, IPC) before claiming parity
2. **State lifecycle review**: For any new `DashMap` or stateful registry, trace all exit paths and verify cleanup
3. **Rollback pattern**: Any insert-before-async-call must handle the failure case
4. **Test both positive and negative capability synthesis**: The test for `resources.subscribe` should cover both "upstream supports it" and "no upstreams" cases

## Related Documentation

- [Phase 2A Notification Infrastructure](phase2a-notification-infrastructure-tools-list-changed-20260307.md) -- foundational notification fan-out pattern
- [Phase 2C Resources, Prompts, Pagination](phase2c-resources-prompts-pagination-20260307.md) -- resource routing and capability synthesis
- [Phase 2B Progress and Cancellation Routing](phase2b-progress-cancellation-routing-20260307.md) -- targeted notification correlation
- [Critical Review Fixes: HTTP Auth & IPC Parity](review-fixes-critical-http-auth-ipc-parity-20260307.md) -- IPC capability parity patterns
- Plan: `docs/plans/2026-03-07-feat-roadmap-tail-closeout-plan.md`
- PR #30: feat(resources): implement subscribe/unsubscribe forwarding

## Files Modified

- `plug-core/src/proxy/mod.rs` -- Subscription registry, lifecycle methods, handlers, tests
- `plug-core/src/server/mod.rs` -- `on_resource_updated()` upstream handler
- `plug-core/src/notifications.rs` -- `ResourceUpdated` variant
- `plug-core/src/http/server.rs` -- HTTP subscribe/unsubscribe handlers, session cleanup
- `plug/src/daemon.rs` -- IPC capability masking and explicit error
