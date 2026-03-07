---
title: "Phase 3C daemon continuity recovery requires active IPC connections to honor daemon shutdown"
category: integration-issues
tags:
  - daemon
  - continuity
  - ipc
  - stdio
  - reconnection
  - integration-tests
  - unix-sockets
module: plug
date: 2026-03-07
symptom: |
  The daemon-backed `plug connect` path already contained reconnect logic, stable client IDs,
  session refresh, and a heartbeat loop, but there was no end-to-end proof that the same downstream
  client could survive a daemon restart. When that proof was added, the first continuity test showed
  that daemon shutdown stopped accepting new IPC connections but left existing proxy IPC connections
  alive. That meant the proxy never observed EOF, never refreshed its daemon session, and kept
  talking to a shutting-down daemon runtime until tool traffic failed.
root_cause: |
  `run_daemon()` listened for the global cancellation token in the accept loop only. Existing
  `handle_ipc_loop()` tasks waited indefinitely on `ipc::read_frame()` for registered proxy
  sessions and did not also select on the daemon cancellation token. As a result, daemon shutdown
  removed the socket file and exited the accept loop, but long-lived proxy connections were not
  terminated, so the continuity machinery in `IpcProxyHandler` had nothing to react to.
severity: high
related:
  - docs/brainstorms/2026-03-07-phase3c-daemon-continuity-recovery-brainstorm.md
  - docs/plans/2026-03-07-feat-phase3c-daemon-continuity-recovery-plan.md
  - /Users/robdezendorf/Documents/GitHub/plug/docs/plans/2026-03-05-feat-daemon-client-session-continuity-plan.md
  - plug/src/daemon.rs
  - plug/src/ipc_proxy.rs
  - plug/src/runtime.rs
---

# Phase 3C daemon continuity recovery requires active IPC connections to honor daemon shutdown

## Problem

The reconnect story at the daemon boundary looked complete on paper:

- stable `client_id`
- protocol-versioned `Register`
- reconnectable `DaemonProxySession`
- heartbeat-based stale-session repair

But the new end-to-end continuity test exposed a real lifecycle bug: the proxy never reconnected
after daemon shutdown because the daemon never actually closed the existing IPC connection.

That produced a misleading half-alive state:

- safe requests could still interact with stale daemon-side state
- session IDs did not change
- the proxy continuity path never engaged

## Investigation

The continuity test was intentionally built with:

- a real daemon socket
- a real downstream stdio client
- a real `IpcProxyHandler`
- a real mock MCP upstream server

The debugging sequence narrowed the failure down in stages:

1. the daemon restarted successfully
2. the restarted engine itself could handle direct tool traffic
3. the downstream proxy session ID stayed unchanged across restart

That last point made the real issue obvious: the old IPC connection stayed open, so the proxy had
no broken-pipe/EOF event to classify as reconnectable transport failure.

## Solution

### 1. Make active IPC connections shutdown-aware

`handle_ipc_loop()` now selects on the daemon cancellation token while waiting for the next frame.

Before:

- registered proxy connections waited forever on `ipc::read_frame(reader).await`
- daemon shutdown only stopped the listener

After:

- both long-lived and short-lived IPC connections break out when daemon cancellation fires
- `handle_ipc_connection()` can auto-deregister the session on disconnect
- the downstream proxy finally sees the transport break and can refresh its session

### 2. Add a test-only daemon runtime-path override

The continuity test needed isolated socket, PID, and token paths without mutating process
environment unsafely in Rust 2024.

The fix was a `#[cfg(test)]` runtime/log path override inside `plug/src/daemon.rs`, used only by
tests. That keeps the continuity test isolated from any real local daemon.

### 3. Prove continuity end to end

The new test validates:

- initial daemon-backed stdio tool traffic works
- daemon shutdown/restart happens under isolated socket paths
- the same downstream client resumes safe proxy traffic after restart
- the daemon session ID is replaced after reconnect

## Verification

This tranche was verified with:

- targeted daemon continuity test in `plug/src/ipc_proxy.rs`
- full workspace test suite
- `cargo clippy --all-targets --all-features -- -D warnings`

## Prevention / Reuse

Two durable lessons came out of this:

1. stopping a listener is not the same thing as shutting down active client sessions
2. continuity features need transport-level proof, not just helper-level logic tests

For future daemon or remote-session work:

- any long-lived connection loop should select on the owning runtime’s shutdown token
- reconnect features should always be validated by checking that the session identifier actually
  changes after recovery

The key compounding lesson is simple: continuity depends as much on clean teardown as it does on
clean reconnect.
