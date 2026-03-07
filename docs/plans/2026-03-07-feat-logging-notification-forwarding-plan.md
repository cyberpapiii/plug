---
title: "feat: Logging & Notification Forwarding"
type: feat
status: active
date: 2026-03-07
parent: docs/plans/2026-03-07-feat-mcp-spec-compliance-roadmap-plan.md
---

# feat: Logging & Notification Forwarding (Phase A1)

## Overview

Forward `notifications/message` (logging) from upstream MCP servers to all connected downstream clients. Add `logging/setLevel` forwarding so downstream clients can control upstream server log levels. Use a **separate broadcast channel** for logging to prevent log volume from causing `Lagged` errors that drop delivery-critical Progress/Cancelled notifications.

## Problem Statement / Motivation

Every MCP SDK emits log notifications by default via `notifications/message`. plug silently drops all of them because `UpstreamClientHandler` doesn't implement `on_logging_message()`. This is the single most impactful protocol correctness gap — logging has the highest server-side adoption rate of any feature plug currently drops.

Without logging forwarding:
- Developers can't debug upstream server issues through plug
- `logging/setLevel` requests from clients silently fail
- plug doesn't advertise `logging` capability, so spec-aware clients may not attempt logging at all

## Proposed Solution

### Architecture: Separate Logging Channel

```
Upstream Server A ──notifications/message──→ UpstreamClientHandler.on_logging_message()
Upstream Server B ──notifications/message──→ UpstreamClientHandler.on_logging_message()
                                                       ↓
                                              prefix logger with server_id
                                                       ↓
                                         ┌─────────────────────────────┐
                                         │ logging_tx: broadcast(512)  │  ← NEW channel
                                         └─────────────────────────────┘
                                              ↓                    ↓
                                    stdio fan-out task    HTTP fan-out task
                                    (per-client)          (daemon-wide)
                                              ↓                    ↓
                                    peer.notify_              sessions.broadcast()
                                    logging_message()         (try_send, non-blocking)

Downstream Client ──logging/setLevel──→ ProxyHandler
                                              ↓
                                   store level in ToolRouter
                                              ↓
                                   peer.set_logging_level() on each healthy upstream
```

Key design decisions:
- **Separate channel** (capacity 512) for logging. Control notifications (Progress, Cancelled, ToolListChanged) stay on existing channel (capacity 128). Rationale: at 10+ upstream servers, logging can produce 100+ msgs/sec at debug level; mixing causes `Lagged` errors that drop delivery-critical notifications.
- **Broadcast to all clients** — logging is not request-scoped (unlike Progress/Cancelled which target a specific client). All connected clients see all server logs.
- **Server-prefixed logger names** — `github:default` instead of `default` so clients can distinguish sources.
- **Default level `warning`** — only forward debug/trace if client explicitly requests via `setLevel`.

## Technical Approach

### rmcp 1.1.0 Types (Confirmed)

```rust
// Already available in rmcp 1.1.0:
LoggingMessageNotificationParam { level: LoggingLevel, logger: Option<String>, data: Value }
LoggingLevel { Debug, Info, Notice, Warning, Error, Critical, Alert, Emergency }
SetLevelRequestParams { meta: Option<Meta>, level: LoggingLevel }

// Peer methods:
peer.notify_logging_message(params)  // Server peer → downstream client
// For upstream: need to check if peer.set_logging_level() exists or if we send raw request
```

### Institutional Learnings Applied

1. **`try_send()` for fan-out** — one slow client must not stall others (from phase2a learning)
2. **`Arc<str>` for string fields in broadcast types** — O(1) clone per subscriber instead of O(n) heap copy (from phase4 learning)
3. **Handle `RecvError::Lagged` gracefully** — log warning, emit synthetic "plug: skipped N log messages" (from phase4 learning)
4. **Prune expired sessions before fan-out** — remove stale receivers immediately (from phase2a learning)

## Implementation Tasks

### Step 1: Add LoggingMessage variant to ProtocolNotification

**File:** `plug-core/src/notifications.rs`

- [ ] Import `LoggingMessageNotificationParam`, `LoggingMessageNotification`, `LoggingLevel` from rmcp
- [ ] Add variant:
  ```rust
  LoggingMessage {
      params: LoggingMessageNotificationParam,
  }
  ```
  Note: No `target` — logging broadcasts to ALL clients (not request-scoped)
- [ ] Add `to_server_jsonrpc_message()` match arm for `LoggingMessage`
- [ ] Verify `to_json_value()` works via the shared serialization path

### Step 2: Add separate logging broadcast channel to ToolRouter

**File:** `plug-core/src/proxy/mod.rs`

- [ ] Add field to `ToolRouter`:
  ```rust
  logging_tx: broadcast::Sender<ProtocolNotification>,
  ```
- [ ] Initialize in `ToolRouter::new()`: `broadcast::channel(512)`
- [ ] Add `subscribe_logging(&self) -> broadcast::Receiver<ProtocolNotification>` method
- [ ] Add `publish_logging(&self, notification: ProtocolNotification)` method
- [ ] Add `current_log_level: ArcSwap<LoggingLevel>` field, default `LoggingLevel::Warning`
- [ ] Add `set_log_level(&self, level: LoggingLevel)` and `log_level(&self) -> LoggingLevel` accessors
- [ ] Add `route_upstream_logging_message(&self, server_id: &str, params: LoggingMessageNotificationParam)` method:
  - Check if `params.level` meets the current threshold (level >= current_log_level)
  - Prefix logger: `params.logger = Some(format!("{server_id}:{}", params.logger.as_deref().unwrap_or("default")))`
  - Publish to logging channel

### Step 3: Implement on_logging_message in UpstreamClientHandler

**File:** `plug-core/src/server/mod.rs`

- [ ] Add import for `LoggingMessageNotificationParam`
- [ ] Implement callback following existing on_progress pattern:
  ```rust
  fn on_logging_message(
      &self,
      params: LoggingMessageNotificationParam,
      _context: NotificationContext<rmcp::RoleClient>,
  ) -> impl Future<Output = ()> + Send + '_ {
      let router = self.router.clone();
      let server_id = Arc::clone(&self.server_id);
      async move {
          if let Some(router) = router.upgrade() {
              router.route_upstream_logging_message(server_id.as_ref(), params);
          }
      }
  }
  ```

### Step 4: Add logging fan-out to stdio consumer

**File:** `plug-core/src/proxy/mod.rs` (notification task around line 1927-1974)

- [ ] Subscribe to logging channel: `let mut log_rx = self.router.subscribe_logging();`
- [ ] Add `tokio::select!` branch in the existing notification task:
  ```rust
  recv = log_rx.recv() => {
      match recv {
          Ok(ProtocolNotification::LoggingMessage { params }) => {
              if peer.notify_logging_message(params).await.is_err() { break; }
          }
          Err(RecvError::Lagged(skipped)) => {
              tracing::warn!(skipped, "stdio logging fan-out lagged");
              // Emit synthetic message to client
              let _ = peer.notify_logging_message(LoggingMessageNotificationParam {
                  level: LoggingLevel::Warning,
                  logger: Some("plug".to_string()),
                  data: serde_json::json!(format!("skipped {skipped} log messages")),
              }).await;
          }
          _ => {}
      }
  }
  ```

### Step 5: Add logging fan-out to HTTP consumer

**File:** `plug-core/src/http/server.rs` (spawn_notification_fanout)

- [ ] Subscribe to logging channel: `let mut log_rx = state.router.subscribe_logging();`
- [ ] Add `tokio::select!` branch in the existing notification task:
  ```rust
  recv = log_rx.recv() => {
      match recv {
          Ok(ref notif @ ProtocolNotification::LoggingMessage { .. }) => {
              state.sessions.broadcast(notif.to_json_value());
          }
          Err(RecvError::Lagged(skipped)) => {
              tracing::warn!(skipped, "HTTP logging fan-out lagged");
          }
          _ => {}
      }
  }
  ```
  Note: `broadcast()` already uses `try_send()` internally (from phase2a hardening)

### Step 6: Forward logging/setLevel from downstream clients

**File:** `plug-core/src/proxy/mod.rs` (ProxyHandler)

- [ ] Handle `logging/setLevel` in the request router (check if rmcp routes this to a specific handler method or if we need to intercept it)
- [ ] When received:
  1. Parse `SetLevelRequestParams` to get the desired `LoggingLevel`
  2. Store in `ToolRouter` via `router.set_log_level(level)`
  3. Fan out to all healthy upstream servers: iterate `server_manager().healthy_servers()`, call `peer.set_logging_level(level)` on each (or send raw JSON-RPC request if no convenience method)
  4. Return success response
- [ ] New upstream server connections should inherit the current log level: after connect, call `set_logging_level` with `router.log_level()`

### Step 7: Advertise logging capability

**File:** `plug-core/src/proxy/mod.rs` (synthesized_capabilities)

- [ ] Add logging capability when any upstream server supports it:
  ```rust
  if upstream_caps.iter().any(|caps| caps.logging.is_some()) {
      capabilities.logging = Some(LoggingCapability {});
  }
  ```
- [ ] Verify the rmcp `ServerCapabilities` struct has a `logging` field and `LoggingCapability` type

### Step 8: Tests

- [ ] Unit test: `route_upstream_logging_message` publishes to logging channel with server-prefixed logger
- [ ] Unit test: `route_upstream_logging_message` filters by current log level (warning+ only by default)
- [ ] Unit test: `set_log_level` changes the threshold and subsequent messages respect it
- [ ] Integration test: upstream server emits log → downstream stdio client receives it with server prefix
- [ ] Integration test: downstream client sends `setLevel(debug)` → upstream servers receive it → debug messages now forwarded
- [ ] Integration test: burst of 200+ log messages does NOT cause Progress/Cancelled `Lagged` on the control channel (separate channels verified)
- [ ] Test: `synthesized_capabilities()` includes logging when any upstream supports it

## System-Wide Impact

- **Interaction graph**: Upstream server → `on_logging_message` → `route_upstream_logging_message` (level filter + prefix) → `logging_tx.send()` → stdio/HTTP fan-out tasks → downstream clients
- **Error propagation**: Logging is fire-and-forget. Channel send failures are logged but never block upstream server communication. `Lagged` errors emit synthetic warning to client.
- **State lifecycle risks**: `current_log_level` in `ToolRouter` persists for the daemon lifetime. If all clients disconnect and reconnect, the level stays at whatever was last set. This is intentional — the level is a server-side concern.
- **API surface parity**: Both stdio and HTTP downstream transports get logging forwarding. Both can send `setLevel`.

## Acceptance Criteria

- [ ] `notifications/message` from any healthy upstream server reaches all connected downstream clients
- [ ] Logger name includes server identifier for disambiguation (format: `server_id:original_logger`)
- [ ] `logging/setLevel` propagates to all upstream servers
- [ ] `logging` capability correctly advertised downstream when any upstream supports it
- [ ] Log volume does not cause loss of Progress/Cancelled signals (separate channels)
- [ ] Default level is `warning` — debug/trace only forwarded after explicit `setLevel`
- [ ] `Lagged` on logging channel emits synthetic warning, does not crash or lose control notifications

## Dependencies & Risks

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| rmcp `peer.set_logging_level()` doesn't exist on client peer | Medium | Medium | Fall back to raw JSON-RPC request construction |
| Logging volume overwhelms 512-capacity channel | Low | Low | Lagged handler emits warning; can increase capacity later |
| New server connections miss initial setLevel | Low | Medium | Apply current level on connection establishment |

## Sources & References

### Parent Plan
- `docs/plans/2026-03-07-feat-mcp-spec-compliance-roadmap-plan.md` — Phase A1

### Internal References
- UpstreamClientHandler: `plug-core/src/server/mod.rs:36-97`
- ProtocolNotification: `plug-core/src/notifications.rs:13-24`
- Broadcast channel: `plug-core/src/proxy/mod.rs:95,188`
- Stdio fan-out: `plug-core/src/proxy/mod.rs:1927-1974`
- HTTP fan-out: `plug-core/src/http/server.rs:43-109`
- synthesized_capabilities: `plug-core/src/proxy/mod.rs:868-893`

### Institutional Learnings
- `docs/solutions/integration-issues/phase2a-notification-infrastructure-tools-list-changed-20260307.md` — try_send fan-out, separate channels
- `docs/solutions/integration-issues/phase4-tui-dashboard-daemon-patterns.md` — Arc<str> for broadcast types, Lagged handling

### rmcp 1.1.0 Types (Confirmed)
- `LoggingMessageNotificationParam` — `{ level: LoggingLevel, logger: Option<String>, data: Value }`
- `LoggingLevel` — `{ Debug, Info, Notice, Warning, Error, Critical, Alert, Emergency }`
- `SetLevelRequestParams` — `{ meta: Option<Meta>, level: LoggingLevel }`
- `peer.notify_logging_message(params)` — exists on `Peer<RoleServer>`
- `on_logging_message()` — exists on `ClientHandler` trait
