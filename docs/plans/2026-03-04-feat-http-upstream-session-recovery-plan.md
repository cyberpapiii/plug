---
title: "feat: HTTP Upstream Session Recovery"
type: feat
status: completed
date: 2026-03-04
---

# feat: HTTP Upstream Session Recovery

## Enhancement Summary

**Deepened on:** 2026-03-04
**Research agents used:** 8 (SpecFlow, Architecture Strategist, Performance Oracle, Security Sentinel, Code Simplicity, Learnings Researcher, backon API, rmcp ServiceError)

### Key Improvements from Research
1. **CRITICAL: rmcp error mapping corrected** — HTTP 404 maps to `ServiceError::TransportSend(DynamicTransportError)`, NOT `McpError`. The original `is_session_error()` was wrong.
2. **Single reconnect codepath** — All reconnects route through `Engine::restart_server()` instead of 3 independent paths (architecture consensus)
3. **Circuit breaker fail-fast** — Replace per-server Mutex with circuit breaker check for stampede prevention (performance + simplicity consensus)
4. **TaskTracker for proactive recovery** — Proactive recovery tasks tracked for clean shutdown (architecture finding)

### New Considerations Discovered
- rmcp is v1.0.0 in Cargo.lock, not 0.16.0 — all code references updated
- No `ServiceError::Transport` variant exists — it's `TransportSend` and `TransportClosed`
- Session ID is internal to rmcp worker — no public API to check staleness
- `DynamicTransportError` wraps `StreamableHttpError::UnexpectedServerResponse` — must string-match on the formatted message

## Overview

Implement reactive and proactive session recovery for HTTP upstream servers in plug. When an upstream Streamable HTTP server's session becomes stale (server restarted, session expired), plug should automatically reconnect and retry transparently — no client restart required.

This was explicitly planned in the Phase 3 resilience spec but never implemented:
- `docs/research/mcp-spec-deep-dive.md:134` — "When an upstream session returns 404, the multiplexer must re-initialize and re-establish state"
- `docs/plans/phase-3-resilience-plan.md:308-341` — backon reconnection code (never built)
- `VISION.md:103`, `USERS.md:87,149,207,239` — self-healing sessions as core promise

## Problem Statement

When an upstream HTTP server restarts (e.g., Workspace MCP Python server), plug's cached `StreamableHttpClientTransport` holds a stale `Mcp-Session-Id`. All subsequent tool calls fail with:

```json
{"jsonrpc":"2.0","id":"server-error","error":{"code":-32600,"message":"Session not found"}}
```

This error propagates to every connected client. Users must manually restart plug or the daemon to recover. With 3-4 Claude Code + 3-4 Codex instances connected simultaneously, a single upstream server restart breaks all of them until manual intervention.

**Current behavior:**
- `proxy/mod.rs:317-339` — errors pass through, no retry/reconnect
- `health.rs:90-133` — detects Failed state, refreshes tool cache, but never reconnects
- `engine.rs:249-301` — `restart_server()` exists but is manual-only (TUI/IPC), rate-limited to 1 per 10s
- `backon = "1.6"` declared in `Cargo.toml` but never imported

## Proposed Solution

Two complementary recovery mechanisms, both funneled through a single reconnect codepath:

### A. Reactive Recovery (tool call path)

When a tool call returns an error that looks like a session/connection failure, reconnect and retry once:

1. Tool call fails with `ServiceError` in `proxy/mod.rs:317`
2. Classify the error — is it a session/transport error or an application error?
3. If session error: call `Engine::reconnect_server()` (new method, reuses `restart_server()` logic without rate limit)
4. Retry the original tool call exactly once
5. If retry fails, return the error normally

### B. Proactive Recovery (health check path)

When health checks detect a server has reached `Failed` state, attempt automatic reconnection:

1. `health.rs` detects `Failed` state transition
2. Spawn a tracked recovery task with exponential backoff via `backon`
3. Each attempt calls `Engine::reconnect_server()` — full initialize handshake
4. On success: server replaced, health state reset, tools refreshed
5. On exhaustion (5 attempts, 1s→60s backoff): log and stop, wait for next health check cycle

## Technical Approach

### Error Classification (CORRECTED from research)

**Critical finding from rmcp v1.0.0 source analysis:**

The rmcp SDK (v1.0.0 per Cargo.lock) has these `ServiceError` variants:

```rust
pub enum ServiceError {
    McpError(McpError),                    // JSON-RPC error frame
    TransportSend(DynamicTransportError),  // Transport-level send failure
    TransportClosed,                       // Transport channel closed
    UnexpectedResponse,                    // Wrong response type
    Cancelled { reason: Option<String> },  // Task cancelled
    Timeout { timeout: Duration },         // Request timeout
}
```

**There is NO `ServiceError::Transport` variant.** The original plan's `is_session_error()` was wrong.

**HTTP 404 "Session not found" propagation path:**
1. Server returns HTTP 404 with body `"Not Found: Session not found"`
2. rmcp client's `post_message` creates `StreamableHttpError::UnexpectedServerResponse("HTTP 404 Not Found: Not Found: Session not found")`
3. Worker wraps it as `ServiceError::TransportSend(DynamicTransportError{...})`
4. plug receives `ServiceError::TransportSend` at `proxy/mod.rs:317`

**No JSON-RPC session error codes exist** — the 404 never reaches the JSON-RPC framing layer. It's always an HTTP transport error.

| Error Type | ServiceError Variant | Action |
|-----------|---------------------|--------|
| HTTP 404 session not found | `TransportSend` (wraps `UnexpectedServerResponse("HTTP 404...")`) | Reconnect + retry |
| Connection refused/reset | `TransportSend` or `TransportClosed` | Reconnect + retry |
| JSON-RPC application error | `McpError` (tool errors, invalid params) | Pass through |
| Timeout | `Timeout` | Pass through (existing behavior) |
| Task cancelled | `Cancelled` | Pass through |

**Implementation:**

```rust
// plug-core/src/proxy/mod.rs — new helper
fn is_session_error(e: &rmcp::service::ServiceError) -> bool {
    use rmcp::service::ServiceError;
    match e {
        // Transport send failures (HTTP 404, connection refused, etc.)
        ServiceError::TransportSend(dyn_err) => {
            let msg = dyn_err.to_string().to_lowercase();
            // HTTP 404 with session-related body = definite session death
            // Connection errors = server likely restarted
            msg.contains("404") || msg.contains("session") || msg.contains("connection")
        }
        // Transport closed = connection dropped
        ServiceError::TransportClosed => true,
        // All other variants: do NOT reconnect
        _ => false,
    }
}
```

**Research insight:** Matching on the `DynamicTransportError.to_string()` output is necessary because `DynamicTransportError` erases the inner type. The rmcp integration test `tests/test_streamable_http_stale_session.rs` confirms the formatted string contains "404" and "session not found" (case-insensitive).

### Stampede Prevention (Simplified from research)

**Performance finding:** A per-server `tokio::sync::Mutex<()>` would serialize ALL tool calls for 2-5 seconds during reconnection. With 8 concurrent clients, this queues everyone.

**Simplified approach:** Use the circuit breaker as a natural stampede preventer:

1. First tool call hits session error → circuit breaker `on_failure()` fires
2. Circuit breaker transitions to Open state after threshold failures
3. Subsequent calls immediately get circuit breaker rejection (fail-fast, no queue)
4. Meanwhile, reactive recovery reconnects in the background
5. On successful reconnect, `replace_server()` resets the circuit breaker
6. Next calls go through to the fresh connection

**Fallback for concurrent calls arriving before circuit breaker opens:** An `AtomicBool` per-server `reconnecting` flag prevents duplicate reconnect attempts. First caller sets it, does the reconnect, clears it. Others see the flag and skip reconnect, just fail fast — they'll succeed on retry since the first caller's reconnect will have completed.

```rust
// plug-core/src/server/mod.rs — add to ServerManager
pub reconnecting: DashMap<String, Arc<AtomicBool>>,
```

### Single Reconnect Codepath (Architecture consensus)

**Critical architecture finding:** Three reconnect codepaths (reactive proxy, proactive health, Engine::restart_server) with no coordination is a recipe for race conditions.

**Solution:** Route ALL reconnects through `Engine`:

```rust
// plug-core/src/engine.rs — new method
/// Reconnect a server without rate limiting. Used by reactive and proactive recovery.
/// Rate limiting is NOT applied here — session recovery should not be throttled.
pub async fn reconnect_server(&self, server_id: &str) -> Result<(), anyhow::Error> {
    // Check AtomicBool — if already reconnecting, return Ok (another path is handling it)
    let reconnecting = self.server_manager.get_reconnecting_flag(server_id);
    if reconnecting.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_err() {
        tracing::debug!(server = %server_id, "reconnect already in progress, skipping");
        return Ok(());
    }

    let result = self.do_reconnect(server_id).await;
    reconnecting.store(false, Ordering::SeqCst);
    result
}

async fn do_reconnect(&self, server_id: &str) -> Result<(), anyhow::Error> {
    let config = self.config.load();
    let server_config = config
        .servers
        .get(server_id)
        .ok_or_else(|| anyhow::anyhow!("unknown server: {server_id}"))?
        .clone();

    match ServerManager::start_server(server_id, &server_config).await {
        Ok(upstream) => {
            self.server_manager.replace_server(server_id, upstream);
            self.tool_router.refresh_tools().await;
            let _ = self.event_tx.send(EngineEvent::ServerStarted {
                server_id: Arc::from(server_id),
            });
            tracing::info!(server = %server_id, "server reconnected");
            Ok(())
        }
        Err(e) => {
            let _ = self.event_tx.send(EngineEvent::Error {
                context: Arc::from("reconnect_server"),
                message: Arc::from(e.to_string().as_str()),
            });
            Err(e)
        }
    }
}
```

The existing `restart_server()` keeps its 10s rate limit for manual TUI/IPC use.

### Reactive Recovery Implementation

In `proxy/mod.rs`, the `ToolRouter` needs access to `Engine` (or a trait) to call `reconnect_server()`. Since `ToolRouter` is created by `Engine`, pass an `Arc<Engine>` or extract a `Reconnector` trait.

**Simplest approach:** Add `engine: Weak<Engine>` to `ToolRouter`, set after Engine construction.

```rust
// proxy/mod.rs — inside execute_tool_call, replace the Ok(Err(e)) arm at line 317
Ok(Err(e)) if is_session_error(&e) && !is_retry => {
    tracing::warn!(
        server = %server_id,
        tool = %original_name,
        error = %e,
        "session error detected, attempting reconnect"
    );

    // Attempt reconnect via Engine (single codepath, AtomicBool prevents stampede)
    if let Some(engine) = self.engine.upgrade() {
        match engine.reconnect_server(&server_id).await {
            Ok(()) => {
                tracing::info!(server = %server_id, "reconnected, retrying tool call");
            }
            Err(reconnect_err) => {
                tracing::error!(
                    server = %server_id,
                    error = %reconnect_err,
                    "reconnect failed, returning original error"
                );
                if let Some(cb) = &cb { cb.on_failure(); }
                return Err(McpError::internal_error(e.to_string(), None));
            }
        }
    } else {
        // Engine dropped — shutting down
        return Err(McpError::internal_error(e.to_string(), None));
    }

    // Retry the tool call exactly once
    self.execute_tool_call_inner(server_id, original_name, arguments, true).await
}
```

**Key design choices:**
- `is_retry` boolean prevents infinite recursion — max 1 reconnect attempt per call
- `AtomicBool` in `reconnect_server()` prevents stampede without queuing
- Health state reset happens inside `replace_server()` (updated to reset both CB and health)
- All reconnects go through `Engine::reconnect_server()`

### Proactive Recovery Implementation

In `health.rs`, when a server transitions to `Failed`, spawn a tracked recovery task:

```rust
// health.rs — after detecting Failed state transition
if new == ServerHealth::Failed {
    let engine = engine.clone();
    let name = name.clone();

    // Use TaskTracker for clean shutdown (architecture finding)
    engine.task_tracker().spawn(async move {
        use backon::{ExponentialBuilder, Retryable};

        let reconnect = || async {
            engine.reconnect_server(&name).await
        };

        match reconnect
            .retry(
                ExponentialBuilder::default()
                    .with_min_delay(Duration::from_secs(1))
                    .with_max_delay(Duration::from_secs(60))
                    .with_max_times(5),
            )
            .sleep(tokio::time::sleep)
            .await
        {
            Ok(()) => {
                tracing::info!(server = %name, "proactive recovery succeeded");
            }
            Err(e) => {
                tracing::error!(
                    server = %name,
                    error = %e,
                    "proactive recovery exhausted (5 attempts), will retry on next health cycle"
                );
            }
        }
    });
}
```

**Research insights applied:**
- `TaskTracker` instead of bare `tokio::spawn` — ensures proactive tasks are awaited during shutdown
- `backon` v1.6 API confirmed: `ExponentialBuilder::default()`, `.with_min_delay()`, `.with_max_delay()`, `.with_max_times()`, `.sleep(tokio::time::sleep)` — requires `tokio-sleep` feature
- Routes through `Engine::reconnect_server()` — single codepath, AtomicBool dedup

### Updated `replace_server()`

```rust
// plug-core/src/server/mod.rs — update replace_server
pub fn replace_server(&self, name: &str, upstream: UpstreamServer) {
    let mut new_map = HashMap::clone(&self.servers.load());
    new_map.insert(name.to_string(), Arc::new(upstream));
    self.servers.store(Arc::new(new_map));
    // Reset circuit breaker
    if let Some(cb) = self.circuit_breakers.get(name) {
        cb.reset();
    }
    // Reset health state (NEW — was missing)
    if let Some(mut entry) = self.health.get_mut(name) {
        *entry = HealthState::new();
    }
}
```

### Interaction Between Reactive and Proactive

Both mechanisms call `Engine::reconnect_server()` which uses `AtomicBool` to prevent concurrent reconnects:

- If proactive recovery reconnects first, reactive retries will use the fresh connection
- If reactive recovery reconnects first, proactive recovery's `reconnect_server()` sees the AtomicBool and returns Ok immediately
- No lock contention, no queuing, no stampede

### Stdio Servers

Stdio servers (e.g., Slack) have different failure modes — the child process crashes rather than returning HTTP 404. The reactive recovery pattern applies identically: if a tool call returns a transport error (`TransportSend` or `TransportClosed`), reconnect (respawn process) and retry. No special-casing needed because `start_server()` handles both transport types.

### DashMap Safety (Institutional learning)

From `docs/solutions/integration-issues/`:
- **Never hold DashMap guards across `.await` points** — extract data, drop guard, then await
- **Clone-and-drop pattern**: `let value = entry.clone(); drop(entry); value.do_async().await;`
- All code in this plan follows this pattern

## Acceptance Criteria

### Functional Requirements

- [x] When an HTTP upstream returns HTTP 404 "Session not found", plug automatically reconnects and retries the tool call
- [x] The retry is transparent to the client — no error propagated if retry succeeds
- [x] Max 1 retry per tool call (no infinite loops)
- [x] Concurrent tool calls hitting the same stale session don't cause reconnect stampede (AtomicBool dedup)
- [x] When health checks detect Failed state, automatic reconnection is attempted with exponential backoff (1s → 60s, 5 attempts)
- [x] Proactive recovery resets health state and circuit breaker on success
- [x] Application-level MCP errors (tool errors, invalid params) do NOT trigger reconnection
- [x] Timeouts do NOT trigger reconnection (preserve existing behavior)
- [x] `replace_server()` resets health state (currently only resets circuit breaker)
- [x] backon crate is properly imported and used (currently unused dependency)
- [x] All reconnects route through `Engine::reconnect_server()` (single codepath)

### Non-Functional Requirements

- [x] No deadlocks: AtomicBool is lock-free, no nested locks
- [x] No performance regression: happy path adds only `is_session_error()` pattern match
- [x] No DashMap guards held across await points
- [x] Proactive recovery tasks tracked via TaskTracker for clean shutdown
- [x] Logging: WARN for reconnect attempts, INFO for success, ERROR for failure
- [x] Works for both HTTP and stdio transport types

### Testing

- [x] Unit test: `is_session_error()` correctly classifies all `ServiceError` variants
- [x] Unit test: health state reset on `replace_server()`
- [x] Unit test: AtomicBool prevents concurrent reconnects
- [ ] Integration test: simulate session error → reconnect → retry succeeds (requires live server mock)
- [ ] Integration test: proactive recovery with backon backoff (requires live server mock)

## Implementation Steps

Files to modify:
- `plug-core/src/proxy/mod.rs` — add `is_session_error()`, add retry arm in error handling
- `plug-core/src/server/mod.rs` — add `reconnecting` DashMap, update `replace_server()` to reset health
- `plug-core/src/engine.rs` — add `reconnect_server()` method, add `task_tracker` field
- `plug-core/src/health.rs` — spawn tracked recovery task on Failed transition, pass Engine ref
- `plug-core/Cargo.toml` — add `backon` with `tokio-sleep` feature

Steps:
- [x] Add `reconnecting: DashMap<String, Arc<AtomicBool>>` to `ServerManager`
- [x] Add `get_reconnecting_flag()` method to `ServerManager`
- [x] Update `replace_server()` to also reset `HealthState`
- [x] Add `task_tracker: TaskTracker` to `Engine` (or use existing if present)
- [x] Add `Engine::reconnect_server()` method (no rate limit, AtomicBool dedup)
- [x] Add `is_session_error()` classification function to `proxy/mod.rs`
- [x] Add `engine: Weak<Engine>` to `ToolRouter`, wire it up after Engine construction
- [x] Refactor `execute_tool_call` to support `is_retry` flag (extract inner method)
- [x] Add reactive reconnect+retry logic on session errors in proxy error handling
- [x] Import `backon` with `tokio-sleep` feature in `health.rs`
- [x] Pass `Engine` (or `Arc<Engine>`) to health check spawn
- [x] Add proactive recovery task spawn on `Failed` transition using TaskTracker
- [x] Add unit tests for `is_session_error()` with all ServiceError variants
- [x] Add unit test for `replace_server()` health state reset
- [ ] Add integration test for reactive recovery flow (requires live server mock)
- [x] Run full test suite + clippy

## Dependencies & Risks

| Risk | Mitigation |
|------|-----------|
| `DynamicTransportError.to_string()` format may change across rmcp versions | Pin rmcp version; the string format is tested in rmcp's own integration tests |
| `Weak<Engine>` in ToolRouter creates circular reference | Weak breaks the cycle; Engine owns ToolRouter, ToolRouter holds Weak<Engine> |
| Proactive recovery may conflict with manual `restart_server()` | AtomicBool dedup — second caller returns Ok immediately |
| backon v1.6 API | Confirmed via research: ExponentialBuilder, Retryable trait, tokio-sleep feature |
| In-flight calls on old connection after reconnect | Old `Peer` handles in-flight calls naturally via their existing oneshot channels; new calls go to new Peer |

## Sources & References

### Internal References
- `docs/research/mcp-spec-deep-dive.md:134` — MCP spec requirement for 404 recovery
- `docs/plans/phase-3-resilience-plan.md:308-341` — original backon reconnection plan
- `plug-core/src/proxy/mod.rs:317-339` — current error handling (no retry)
- `plug-core/src/health.rs:90-133` — current health check (no reconnection)
- `plug-core/src/server/mod.rs:422-433` — `replace_server()` (already exists)
- `plug-core/src/engine.rs:249-301` — manual `restart_server()` (rate-limited)
- `VISION.md:103` — self-healing as core principle
- `USERS.md:87,149,207,239` — user stories requiring session recovery

### Research Findings
- rmcp v1.0.0 `ServiceError` variants: `McpError`, `TransportSend`, `TransportClosed`, `UnexpectedResponse`, `Cancelled`, `Timeout`
- HTTP 404 propagation: `StreamableHttpError::UnexpectedServerResponse` → `ServiceError::TransportSend(DynamicTransportError)`
- rmcp test confirmation: `tests/test_streamable_http_stale_session.rs` validates 404 string format
- backon v1.6 API: `ExponentialBuilder::default().with_min_delay().with_max_delay().with_max_times()`, `.retry()`, `.sleep(tokio::time::sleep)`
- Institutional learnings: ArcSwap clone-modify-swap, DashMap guard lifetime rules, list_all_tools as health probe
