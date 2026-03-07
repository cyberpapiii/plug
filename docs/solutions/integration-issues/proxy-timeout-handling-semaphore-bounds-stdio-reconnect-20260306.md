---
title: "Stdio tool-call timeout and reconnect semantics under contention"
category: integration-issues
tags:
  - stdio
  - timeout
  - reconnect
  - semaphore
  - concurrency
  - reliability
  - protocol
  - code-review
module: plug-core
date: 2026-03-06
symptom: |
  With `max_concurrent = 1`, a slow stdio tool call could block later calls indefinitely while timeout handling on the active call could also leave an orphaned JSON-RPC response in the stream. In practice this showed up as silent hangs, inflated tail latency, and risk of the next request reading a stale response after a timed-out call.
root_cause: |
  The tool-call timeout only wrapped `peer.call_tool()` and not semaphore acquisition, so queue wait had no bound. When a stdio call timed out, dropping the future did not stop upstream processing, which could leave protocol state dirty until the transport was reset. The first reconnect fix also awaited recovery inline and used a fixed semaphore budget, so review follow-ups were needed to move reconnect off the caller path and tie queue wait to `call_timeout_secs`.
severity: high
related:
  - todo-024
  - todo-026
  - todo-036
  - todo-037
  - docs/brainstorms/2026-03-06-timeout-semantics-brainstorm.md
  - docs/plans/2026-03-06-feat-proactive-transport-recovery-plan.md
  - docs/plans/2026-03-06-fix-semaphore-acquisition-timeout-plan.md
  - plug-core/src/proxy/mod.rs
  - plug-core/src/error.rs
  - plug-core/tests/integration_tests.rs
---

# Stdio tool-call timeout and reconnect semantics under contention

## Problem

Two coupled resilience bugs existed in the stdio tool-call path:

1. semaphore acquisition was unbounded, so a saturated server could hang callers forever before the configured call timeout even began
2. a timed-out stdio call could keep running upstream and later write an orphaned response into the shared stdio stream, poisoning the next request

This was most obvious on servers with `max_concurrent = 1` and slow tools.

## Investigation

The important code path was in `ToolRouter::call_tool_inner()`:

- queue wait happened at `sem.acquire_owned().await`
- the configured timeout only wrapped `peer.call_tool(...)`
- on stdio timeout, the caller got a timeout error but the underlying process/session was left in place

That meant:

- queueing could hang indefinitely under contention
- timed-out stdio sessions could become unsafe to reuse
- the first reconnect-on-timeout attempt also had a latency bug because it awaited reconnect work inline before returning the timeout

## Solution

The final fix split capacity waiting and execution into separate resilience stages.

### 1. Bound semaphore acquisition

Semaphore acquisition now uses a timeout derived from the server’s own `call_timeout_secs` when available:

```rust
let semaphore_timeout = self
    .server_manager
    .get_upstream(&server_id)
    .map(|upstream| Duration::from_secs(upstream.config.call_timeout_secs))
    .unwrap_or(Duration::from_secs(30));

let permit = if let Some(sem) = self.server_manager.semaphores.get(&server_id) {
    Some(
        tokio::time::timeout(semaphore_timeout, sem.clone().acquire_owned())
            .await
            .map_err(|_| {
                McpError::from(ProtocolError::ServerBusy {
                    server_id: server_id.clone(),
                })
            })?
            .map_err(|_| {
                McpError::from(ProtocolError::ServerUnavailable {
                    server_id: server_id.clone(),
                })
            })?,
    )
} else {
    None
};
```

This turns a silent hang into an explicit overload error.

### 2. Add an explicit overload error

`ProtocolError` gained a dedicated overload case:

```rust
ServerBusy { server_id: String }
```

with message:

```text
server overloaded while waiting for capacity: <server_id>
```

### 3. Reconnect stdio servers after timeout

When a stdio tool call times out, `plug` now schedules a reconnect in the background:

```rust
if matches!(transport_type, crate::config::TransportType::Stdio) {
    self.reconnect_server_in_background(server_id.clone());
}

Err(McpError::from(ProtocolError::Timeout {
    duration: timeout_duration,
}))
```

This is intentionally different from the session-error path:

- session/transport errors still do immediate reconnect-and-retry
- plain timeouts still return a timeout promptly to the caller
- stdio timeout recovery happens off the caller path so timeout latency does not silently expand

### 4. Deduplicate reconnect orchestration

The branch also factored reconnect handling through shared helpers:

- `upgrade_engine()`
- `reconnect_server_now()`
- `reconnect_server_in_background()`

That keeps timeout recovery and session-error recovery from drifting apart.

## Why It Works

The fix restores two invariants:

1. capacity waiting is bounded and explicit
2. stdio timeout is treated as transport hygiene risk, not just as a slow request

The net result:

- slow queueing no longer hangs forever
- overload is surfaced clearly
- timed-out stdio sessions are repaired before later calls trust them again
- caller-visible timeout stays prompt
- timeouts still do not trip the circuit breaker

## Verification

Focused tests added or used:

- `call_tool_times_out_waiting_for_semaphore()`
  Verifies a blocked semaphore returns an overload error instead of waiting forever.

- `test_stdio_timeout_reconnects_cleanly()`
  Uses a wrapper script plus `mock-mcp-server` to force:
  - first stdio server instance is slow enough to time out
  - reconnect occurs
  - second call succeeds on the fresh process

Validation commands used:

```bash
cargo test -p plug-core proxy
cargo test -p plug-core test_stdio_timeout_reconnects_cleanly -- --nocapture
cargo check
```

## Related Prior Work

- [phase3-resilience-token-efficiency.md](./phase3-resilience-token-efficiency.md)
  The resilience subsystem origin: semaphores, circuit breakers, timeout handling.

- [mcp-multiplexer-http-transport-phase2.md](./mcp-multiplexer-http-transport-phase2.md)
  Reinforces the transport-agnostic router boundary.

- [2026-03-04-feat-http-upstream-session-recovery-plan.md](/Users/robdezendorf/.config/superpowers/worktrees/plug/feat-v0-1-stabilization/docs/plans/2026-03-04-feat-http-upstream-session-recovery-plan.md)
  Prior reconnect architecture that established the single reconnect codepath.

- [2026-03-06-feat-proactive-transport-recovery-plan.md](/Users/robdezendorf/.config/superpowers/worktrees/plug/feat-v0-1-stabilization/docs/plans/2026-03-06-feat-proactive-transport-recovery-plan.md)
  Broader continuity strategy that this fix fits under.

## Prevention

- Treat tool-call resilience as a pipeline: health, circuit breaker, queue wait, execution, recovery.
- Never leave semaphore acquisition unbounded.
- Use server-local timeout budgets for queue wait unless there is a deliberate reason not to.
- Do not count tool-call timeouts as circuit-breaker failures by default.
- Treat stdio timeout as protocol-corruption risk and repair the connection before reuse.
- Keep timeout-triggered reconnect best-effort and off the caller path.
- Prefer integration tests for stdio corruption risks; unit tests alone are not enough.
