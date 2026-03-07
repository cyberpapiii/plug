---
title: "MCP logging notification forwarding (Phase A1)"
category: integration-issues
tags: [mcp, logging, notifications, broadcast-channel, bulkhead, fan-out, rmcp, tokio]
module: plug-core
symptom: "Upstream MCP server log messages silently dropped; logging/setLevel requests from clients fail silently"
root_cause: "UpstreamClientHandler did not implement on_logging_message(); no logging broadcast channel existed; logging capability not advertised"
date_solved: 2026-03-07
severity: high
complexity: medium
reuse_potential: high
---

# MCP Logging Notification Forwarding (Phase A1)

## Problem

Every MCP SDK emits log notifications via `notifications/message` by default. plug silently dropped all of them because `UpstreamClientHandler` didn't implement `on_logging_message()`. Additionally:

- `logging/setLevel` requests from downstream clients failed silently
- plug didn't advertise the `logging` capability, so spec-aware clients wouldn't attempt logging
- No infrastructure existed to broadcast log messages to all connected clients

This was the single most impactful protocol correctness gap — logging has the highest server-side adoption rate of any feature plug was dropping.

## Investigation

1. Confirmed `UpstreamClientHandler` in `plug-core/src/server/mod.rs` had `on_progress` and `on_cancelled` handlers but no `on_logging_message`
2. Identified that mixing logging into the existing control notification channel (capacity 128) would cause `Lagged` errors under log-heavy upstream servers, dropping delivery-critical Progress/Cancelled notifications
3. Verified rmcp 1.1.0 types: `LoggingMessageNotificationParam { level, logger, data }`, `LoggingLevel` enum (no `PartialOrd`), `SetLevelRequestParams`, `peer.notify_logging_message()`, `peer.set_level()`
4. Confirmed `ServerCapabilities.logging` is `Option<JsonObject>` (empty `{}` to advertise)

## Root Cause

Missing implementation across four layers:

1. **Capture**: No `on_logging_message` handler on the upstream client
2. **Channel**: No broadcast channel for logging notifications
3. **Fan-out**: No consumer tasks for stdio, HTTP, or IPC transports
4. **Control**: No `setLevel` forwarding or capability advertisement

## Solution

### Architecture: Bulkhead Pattern with Separate Channels

```
Upstream Server A ──notifications/message──→ on_logging_message()
Upstream Server B ──notifications/message──→ on_logging_message()
                                                     ↓
                                            prefix logger with server_id
                                            filter by effective log level
                                                     ↓
                                       ┌─────────────────────────────┐
                                       │ logging_tx: broadcast(512)  │  ← separate channel
                                       └─────────────────────────────┘
                                       ↓            ↓                ↓
                              stdio fan-out  HTTP fan-out   IPC daemon push
                                       ↓            ↓                ↓
                              peer.notify_   sessions.      LoggingNotification
                              logging_       broadcast()    frame via Unix socket
                              message()
```

### Key Design Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Channel separation | Dedicated logging channel (512) vs control (128) | At 10+ upstream servers, logging can produce 100+ msgs/sec at debug level; mixing causes `Lagged` errors that drop Progress/Cancelled |
| Broadcast scope | All clients receive all logs | Logging is not request-scoped (unlike Progress/Cancelled) |
| Logger naming | `server_id:original_logger` prefix | Clients can distinguish log sources |
| Default level | `Warning` | Only forward debug/trace after explicit `setLevel` |
| Multi-client setLevel | Most permissive (lowest severity) wins | Logging is observability; over-deliver beats under-deliver; clients filter locally |
| Level recalculation | On disconnect, recalculate; empty → Warning | Prevents stale debug-level forwarding after all debug clients disconnect |
| Upstream forwarding | Concurrent via `join_all` with capability filtering | Only send to servers that advertise logging capability |
| IPC push notifications | `LoggingNotification` frames interleaved with request-response | Extends request-response IPC protocol with daemon-initiated push; client reads loop handles interleaved frames; heartbeat (1s) ensures sub-second delivery during idle |
| Session expiry cleanup | Callback on all removal paths (cleanup task, validate, prune) | Prevents stale per-client log levels from keeping effective level permanently permissive |

### Implementation: 7 Code Patterns

#### 1. LoggingMessage Variant (`notifications.rs`)

```rust
pub enum ProtocolNotification {
    // ... existing variants ...
    LoggingMessage {
        params: LoggingMessageNotificationParam,
    },
}
```

No `target` field — logging broadcasts to ALL clients, unlike Progress/Cancelled which are request-scoped.

#### 2. Separate Broadcast Channel (`proxy/mod.rs`)

```rust
// In ToolRouter::new()
let (logging_tx, _) = broadcast::channel(512);

// Fields
logging_tx: broadcast::Sender<ProtocolNotification>,
client_log_levels: DashMap<Arc<str>, LoggingLevel>,
effective_log_level: ArcSwap<LoggingLevel>,  // default: Warning
```

#### 3. Level Severity Mapping (`proxy/mod.rs`)

rmcp's `LoggingLevel` doesn't implement `PartialOrd`. Manual mapping:

```rust
fn level_severity(level: &LoggingLevel) -> u8 {
    match level {
        LoggingLevel::Debug => 0,
        LoggingLevel::Info => 1,
        LoggingLevel::Notice => 2,
        LoggingLevel::Warning => 3,
        LoggingLevel::Error => 4,
        LoggingLevel::Critical => 5,
        LoggingLevel::Alert => 6,
        LoggingLevel::Emergency => 7,
    }
}
```

#### 4. Upstream Log Routing with Server Prefix (`proxy/mod.rs`)

```rust
pub fn route_upstream_logging_message(
    &self, server_id: &str, mut params: LoggingMessageNotificationParam
) {
    if Self::level_severity(&params.level) < Self::level_severity(&self.effective_log_level.load()) {
        return;  // below threshold
    }
    // Prefix logger: "github:default" instead of "default"
    params.logger = Some(format!(
        "{}:{}", server_id, params.logger.as_deref().unwrap_or("default")
    ));
    let _ = self.logging_tx.send(ProtocolNotification::LoggingMessage { params });
}
```

#### 5. Concurrent setLevel Forwarding (`proxy/mod.rs`)

```rust
pub async fn forward_set_level_to_upstreams(&self) {
    let level = self.log_level();
    let params = SetLevelRequestParams::new(level);
    let upstreams = self.server_manager.healthy_upstreams();
    let futures: Vec<_> = upstreams.into_iter()
        .filter(|(_, upstream)| upstream.capabilities.logging.is_some())
        .map(|(name, upstream)| {
            let params = params.clone();
            async move {
                if let Err(e) = upstream.client.peer().set_level(params).await {
                    tracing::warn!(server = %name, error = %e, "failed to forward setLevel");
                }
            }
        })
        .collect();
    futures::future::join_all(futures).await;
}
```

#### 6. Stdio Fan-Out with Lagged Recovery (`proxy/mod.rs`)

```rust
// Separate task for logging (alongside existing control notification task)
let mut log_rx = self.router.subscribe_logging();
tokio::spawn(async move {
    loop {
        match log_rx.recv().await {
            Ok(ProtocolNotification::LoggingMessage { params }) => {
                if peer.notify_logging_message(params).await.is_err() { break; }
            }
            Err(RecvError::Lagged(skipped)) => {
                tracing::warn!(skipped, "stdio logging fan-out lagged");
                let _ = peer.notify_logging_message(LoggingMessageNotificationParam {
                    level: LoggingLevel::Warning,
                    logger: Some("plug".to_string()),
                    data: serde_json::json!(format!("skipped {skipped} log messages")),
                }).await;
            }
            Err(RecvError::Closed) => break,
            _ => {}
        }
    }
});
```

#### 7. Capability Advertisement (`proxy/mod.rs`)

```rust
// In synthesized_capabilities()
if merged.iter().any(|(_, caps)| caps.logging.is_some()) {
    let mut logging_obj = serde_json::Map::new();
    capabilities.logging = Some(logging_obj);
}
```

### Review Fixes Applied

Three findings from the 6-agent code review were fixed before merge:

1. **P1 — HTTP session log level leak**: `delete_mcp` handler wasn't calling `remove_client_log_level()` on session termination. Added cleanup call to prevent stale per-client levels from skewing the effective threshold.

2. **P2 — Sequential upstream forwarding**: Original `forward_set_level_to_upstreams` used a sequential loop. Refactored to concurrent `join_all` with capability filtering — only sends to servers that advertise logging support.

3. **P3 — Initial log level sync**: New upstream connections now inherit the current effective log level on startup (with capability check), preventing a window where upstream servers would use their default level.

### Codex Review Fixes

Three additional findings from external review:

1. **High — IPC proxy logging parity**: Added `LoggingNotification` IPC push variant. Daemon subscribes to logging channel after client registration and pushes notifications interleaved with responses. Client-side `try_round_trip_locked` loops to handle interleaved notification frames and forwards them to the downstream peer. Full transport parity across stdio, HTTP, and daemon-backed IPC.

2. **Medium — HTTP session expiry log level leak**: Added `ExpiryCallback` to `StatefulSessionStore` — fires on all session removal paths (cleanup task retain, validate timeout, broadcast prune). Wired to `remove_client_log_level()` at startup.

3. **Low — HTTP lagged logging synthetic message**: HTTP logging fan-out now broadcasts synthetic `plug: skipped N log messages` warning to all clients on `Lagged`, matching stdio behavior.

## Prevention

### Bulkhead Pattern for Broadcast Channels

When adding new notification types to broadcast infrastructure:

- **Always evaluate volume**: High-volume notifications (logging, metrics) get their own channel. Control notifications (ToolListChanged, Progress, Cancelled) stay on the existing channel.
- **Capacity sizing**: Control channel at 128 for bursty-but-rare signals. Logging at 512 for sustained throughput.
- **Lagged recovery**: Always handle `RecvError::Lagged` — emit a synthetic warning to the client rather than silently dropping.

### Session Lifecycle Cleanup

When adding per-client state (like `client_log_levels`):

- **Always clean up on disconnect**: All transports (stdio disconnect, HTTP `DELETE /mcp`, HTTP session expiry, IPC deregister/disconnect) must remove the client's state.
- **Use expiry callbacks for cross-cutting cleanup**: When session stores don't have access to domain state (like log levels), use callback hooks rather than coupling the session store to domain types.
- **Recalculate derived state**: After removal, recalculate any aggregate (like effective log level). Default to safe values when empty.

### Upstream Capability Gating

When forwarding requests to upstream servers:

- **Check capabilities before sending**: Don't send `setLevel` to servers that don't advertise `logging`. This prevents unnecessary error handling for expected failures.
- **Initial sync on connect**: New upstream connections should inherit current state (log level, etc.) rather than relying on the client to re-send.

### Testing Checklist for Notification Features

- [ ] Message routing publishes to the correct channel
- [ ] Level/threshold filtering works as expected
- [ ] Multi-client semantics resolve correctly (most permissive wins)
- [ ] Channel separation verified — high volume on one channel doesn't cause Lagged on another
- [ ] Capability advertisement matches upstream availability
- [ ] Session cleanup removes per-client state

## Files Changed

| File | Changes |
|------|---------|
| `plug-core/src/notifications.rs` | Added `LoggingMessage` variant, serialization arm |
| `plug-core/src/proxy/mod.rs` | Logging channel, level management, fan-out tasks, setLevel handler, capability advertisement, 6 unit tests |
| `plug-core/src/server/mod.rs` | `on_logging_message` handler, `healthy_upstreams()`, initial log level sync |
| `plug-core/src/http/server.rs` | HTTP logging fan-out task, setLevel request handling, session cleanup, synthetic lag warning |
| `plug-core/src/ipc.rs` | Added `LoggingNotification` variant to `IpcResponse` |
| `plug-core/src/session/stateful.rs` | Expiry callback on all session removal paths |
| `plug/src/daemon.rs` | IPC logging push (subscribe + select + drain), `logging/setLevel` dispatch, log level cleanup on deregister/disconnect |
| `plug/src/ipc_proxy.rs` | Peer storage for notification forwarding, interleaved frame handling in `try_round_trip_locked`, `set_level` handler |
| `plug/src/runtime.rs` | Expiry callback wiring for log level cleanup |

## Related

- [Phase 2a notification infrastructure](phase2a-notification-infrastructure-tools-list-changed-20260307.md) — `try_send` fan-out pattern, separate channels concept
- [Phase 4 TUI daemon patterns](../integration-issues/phase4-tui-dashboard-daemon-patterns.md) — `Arc<str>` for broadcast types, Lagged handling
- [MCP spec compliance roadmap](../../plans/2026-03-07-feat-mcp-spec-compliance-roadmap-plan.md) — Phase A1
- [Phase A1 plan](../../plans/2026-03-07-feat-logging-notification-forwarding-plan.md) — Detailed implementation plan
