---
title: "Post-v0.2 upstream restart recovery was already correct; it needed an end-to-end proof"
category: integration-issues
tags:
  - stdio
  - reconnect
  - upstream-restart
  - recovery
  - integration-tests
  - reliability
module: plug-core
date: 2026-03-07
symptom: |
  After `v0.2.0`, the runtime already contained reactive reconnect logic for upstream stdio session
  failures, but there was no end-to-end proof that a real upstream crash/restart was actually
  survivable. That left an important reliability claim inferred from helpers rather than validated
  through the actual tool-call path.
root_cause: |
  The reconnect behavior already existed in `ToolRouter::call_tool_inner()`, but the test suite had
  proof for timeout-driven reconnects rather than crash-driven restarts. The missing asset was an
  end-to-end harness that forced a real first-run crash and then a clean second-run process, not a
  missing runtime capability.
severity: medium
related:
  - docs/brainstorms/2026-03-07-post-v0-2-upstream-restart-recovery-brainstorm.md
  - docs/plans/2026-03-07-feat-post-v0-2-upstream-restart-recovery-plan.md
  - plug-core/tests/integration_tests.rs
  - plug-core/src/proxy/mod.rs
  - docs/solutions/integration-issues/proxy-timeout-handling-semaphore-bounds-stdio-reconnect-20260306.md
---

# Post-v0.2 upstream restart recovery was already correct; it needed an end-to-end proof

## Problem

The runtime already had:

- session/transport failure classification
- reconnect-on-session-error in the tool-call path
- an upstream replacement path through `Engine::reconnect_server()`

What it did not have was proof that a real stdio upstream crash could go through that full path and
restore usable tool traffic.

## Solution

### 1. Use a wrapper script instead of changing the mock server design

The smallest reliable harness was:

- first process runs `mock-mcp-server --fail-mode crash`
- second process runs normal `mock-mcp-server`

That forced the reconnect path to exercise a real child-process crash and a real restarted process
without requiring extra complexity in the harness binary.

### 2. Prove recovery through the real tool-call boundary

The new integration test:

- starts the engine with the wrapper-backed upstream
- calls `Mock__echo`
- lets the first upstream process crash
- allows the reconnect path to start the second process
- verifies the original call recovers and returns usable output

This proves the actual runtime path instead of only testing helper logic or error classification.

## Outcome

The important result was that no runtime fix was required for this tranche.

The reconnect path already behaved correctly; the code just lacked end-to-end evidence.

That is still a valuable result, because it converts a reliability assumption into a maintained
test-backed guarantee.

## Verification

Validated with:

- focused crash/restart recovery integration test
- full workspace test suite
- `cargo clippy --all-targets --all-features -- -D warnings`

## Prevention / Reuse

When a recovery path already exists, the next step should often be proof before redesign.

Two reusable lessons:

1. wrapper scripts are an effective way to model phased child-process behavior in integration tests
2. crash recovery and timeout recovery should each have their own explicit proof, even when they
   share most of the reconnect machinery
