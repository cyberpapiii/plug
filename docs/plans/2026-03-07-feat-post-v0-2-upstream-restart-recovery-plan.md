---
title: "feat: post v0.2 upstream restart recovery proof"
type: feat
status: completed
date: 2026-03-07
origin: docs/brainstorms/2026-03-07-post-v0-2-upstream-restart-recovery-brainstorm.md
---

# Post-v0.2 Upstream Restart Recovery Proof

## Overview

Add the first end-to-end proof that a real upstream stdio server crash is recoverable through
`plug`’s reconnect path without manual intervention.

## Problem Statement / Motivation

The runtime already contains reconnect logic on both the reactive tool-call path and the proactive
health path, but there is not yet a strong proof that a real upstream stdio crash/restart is
survivable.

That leaves an important reliability story inferred from helpers instead of validated through the
actual runtime path.

## Proposed Solution

Build one narrow integration test that:

- starts an upstream stdio server through a small wrapper script
- forces the first process to crash on the first tool call
- lets the next process start cleanly
- verifies `plug` reconnects and restores usable tool traffic

If that test exposes a real recovery gap, fix the runtime in the smallest possible way.

## Technical Considerations

- Reuse the existing `mock-mcp-server`
- Prefer wrapper-script behavior selection over adding more complexity to the harness binary
- Keep the assertion centered on recovered tool traffic, not on internal helper state
- Avoid expanding into proactive health-loop choreography unless the reactive path itself is broken

## System-Wide Impact

- **Interaction graph**: downstream tool call -> `ToolRouter::call_tool_inner()` ->
  upstream crash -> reconnect path -> restarted upstream -> retried/successful tool call.
- **Error propagation**: session/transport failure must still be classified as reconnectable rather
  than surfacing as a final unrecoverable protocol error.
- **State lifecycle risks**: the failed upstream session must be replaced cleanly without leaving a
  poisoned stdio transport behind.
- **API surface parity**: this tranche is stdio-upstream focused; it does not change downstream
  surface area.
- **Integration test scenarios**:
  - first upstream process crashes on call
  - reconnect path starts fresh upstream
  - tool traffic becomes usable again without manual repair

## Acceptance Criteria

- [x] Add a wrapper-script crash/restart test harness for the mock upstream server
- [x] Add one end-to-end upstream restart recovery test
- [x] Keep the test deterministic enough to pass in the normal full suite
- [x] If the proof exposes a runtime gap, fix it in the smallest viable way
- [x] Full suite passes after the proof/fix lands

## Sources & References

- **Origin brainstorm:** `docs/brainstorms/2026-03-07-post-v0-2-upstream-restart-recovery-brainstorm.md`
- `plug-core/src/proxy/mod.rs`
- `plug-core/src/engine.rs`
- `plug-core/src/health.rs`
- `plug-core/tests/integration_tests.rs`
- `docs/solutions/integration-issues/proxy-timeout-handling-semaphore-bounds-stdio-reconnect-20260306.md`
