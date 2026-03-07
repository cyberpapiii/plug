---
title: "feat: phase 3c daemon continuity recovery"
type: feat
status: completed
date: 2026-03-07
origin: docs/brainstorms/2026-03-07-phase3c-daemon-continuity-recovery-brainstorm.md
---

# Phase 3C Daemon Continuity Recovery

## Overview

Add end-to-end verification for daemon-backed `plug connect` continuity so the reconnecting IPC
proxy path is proven under a real daemon restart instead of only inferred from unit-level logic.

## Problem Statement / Motivation

The daemon continuity machinery now exists in production code:

- stable `client_id` registration
- protocol-version checks
- heartbeat-based stale-session detection
- reconnectable session refresh in `IpcProxyHandler`

What is still missing is a transport-level test that proves a downstream stdio client can stay
connected to the proxy, survive a daemon restart, and continue invoking tools through the recovered
daemon session.

Without that, one of the project’s most important reliability promises still relies on reasoning
instead of proof.

## Proposed Solution

Build a narrow daemon continuity verification slice with two parts:

1. add a small test-only runtime/log-path override for daemon socket files so daemon tests can run
   in isolated temp directories without unsafe environment mutation
2. add a real end-to-end continuity test for the daemon-backed IPC proxy path

The continuity test should:

- start a real engine and real daemon
- connect a real downstream stdio client through `IpcProxyHandler`
- prove initial tool traffic works
- restart the daemon
- allow the reconnect path to repair the daemon session
- prove the same downstream client can resume safe proxy traffic and that the daemon session ID changed

## Technical Considerations

- Keep runtime-path overrides `#[cfg(test)]` only
- Avoid spawning the full CLI binary for the daemon when the in-process daemon entrypoint already
  exists
- Reuse the mock MCP server harness for upstream traffic so the only new complexity is the daemon
  boundary
- Prefer one high-value continuity test over multiple flaky variants

## System-Wide Impact

- **Interaction graph**: downstream stdio client -> `IpcProxyHandler` -> daemon IPC -> shared
  engine -> upstream mock server -> response back through recovered IPC session.
- **Error propagation**: daemon restart should surface as a reconnectable transport failure to the
  proxy, then heal before the next user-visible request path fails permanently.
- **State lifecycle risks**: restart must replace daemon session state without leaving the proxy on
  a stale session ID.
- **API surface parity**: this tranche is intentionally daemon/stdio-specific; HTTP continuity is
  deferred.
- **Integration test scenarios**:
  - initial daemon-backed stdio tool call succeeds
  - daemon restarts while downstream client stays attached
  - recovered proxy call succeeds without creating a new downstream client process

## Acceptance Criteria

- [x] Add a test-only isolated daemon runtime/log path override
- [x] Add an end-to-end daemon continuity test for the IPC proxy path
- [x] Verify initial tool traffic succeeds before restart and safe proxy traffic resumes after restart
- [x] Keep the test deterministic enough to pass in the normal suite
- [x] Full suite passes with the new continuity verification in place

## Success Metrics

- The codebase has at least one real transport-level proof of daemon continuity
- Future changes to daemon registration/reconnect logic break a test instead of relying on manual
  smoke validation

## Dependencies & Risks

- Daemon socket cleanup must complete before restart or the second daemon bind will fail
- The test must avoid global runtime-dir collisions with a real local daemon
- Heartbeat timing is part of the product behavior, so the test must allow enough time for repair
  without becoming flaky

## Sources & References

- **Origin brainstorm:** `docs/brainstorms/2026-03-07-phase3c-daemon-continuity-recovery-brainstorm.md`
- `/Users/robdezendorf/Documents/GitHub/plug/docs/plans/2026-03-05-feat-daemon-client-session-continuity-plan.md`
- `plug/src/ipc_proxy.rs`
- `plug/src/runtime.rs`
- `plug/src/daemon.rs`
- `plug-test-harness/src/bin/mock-server.rs`
