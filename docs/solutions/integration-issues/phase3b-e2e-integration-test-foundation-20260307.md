---
title: "Phase 3B end-to-end integration test foundation for stdio, HTTP, and shared upstream clients"
category: integration-issues
tags:
  - integration-tests
  - stdio
  - http
  - sse
  - multi-client
  - test-harness
  - shared-upstream
module: plug-core
date: 2026-03-07
symptom: |
  By the end of Phase 3A, plug had strong subsystem coverage for routing, notifications,
  progress/cancellation, meta-tool mode, and HTTP session behavior, but very little proof that the
  full proxy path behaved correctly when driven like a real client. The existing
  `plug-core/tests/integration_tests.rs` suite was still mostly config and low-level runtime
  verification rather than actual end-to-end proxy execution.
root_cause: |
  The test suite evolved around component boundaries first, which made Phase 1 and Phase 2 work
  land quickly, but left the highest-value end-to-end paths under-tested. There was already a real
  mock MCP server harness, but the integration tests were not yet using it to drive stdio, HTTP,
  and shared-engine multi-client flows through production request paths.
severity: medium
related:
  - docs/brainstorms/2026-03-07-phase3b-e2e-integration-tests-brainstorm.md
  - docs/plans/2026-03-07-feat-phase3b-e2e-integration-tests-plan.md
  - docs/solutions/integration-issues/phase2a-notification-infrastructure-tools-list-changed-20260307.md
  - docs/solutions/integration-issues/phase2b-progress-cancellation-routing-20260307.md
  - docs/solutions/integration-issues/phase3a-meta-tool-mode-tool-drift-20260307.md
  - plug-core/tests/integration_tests.rs
  - plug-test-harness/src/bin/mock-server.rs
---

# Phase 3B end-to-end integration test foundation for stdio, HTTP, and shared upstream clients

## Problem

The codebase already had good local confidence, but not enough boundary confidence.

Specifically, there was no dedicated test layer proving that:

1. a stdio downstream client can initialize, list tools, and call a real routed upstream tool
2. an HTTP downstream client can initialize, attach SSE, list tools, and call a real routed
   upstream tool
3. two downstream clients sharing one engine do not receive each other’s results

That left an uncomfortable gap: most behavior was “likely correct” because the underlying pieces
were tested, but the full proxy path was not being exercised as a user would exercise it.

## Solution

### 1. Reuse the real mock MCP server harness

Instead of creating another fake abstraction, the tests now use the existing
`plug-test-harness` mock server through real child-process startup:

- command: `cargo run --quiet -p plug-test-harness --bin mock-mcp-server -- ...`

This matters because it exercises the real stdio transport and startup path that production uses.

### 2. Add a real stdio end-to-end proxy test

The stdio test now:

- starts a real `Engine` from config
- creates a `ProxyHandler` from the shared router
- serves it over a duplex transport
- connects a real rmcp downstream client
- calls `list_all_tools()`
- invokes a routed tool and checks the returned payload

This validates the actual stdio request flow rather than directly calling `ToolRouter`.

### 3. Add a real HTTP end-to-end proxy test

The HTTP test now:

- starts a real `Engine`
- builds the real Axum MCP router with `HttpState`
- POSTs `initialize`
- opens the SSE stream and verifies the priming event
- POSTs `tools/list`
- POSTs `tools/call`

This proves HTTP parity through the real downstream transport and server stack.

### 4. Add shared-engine multi-client isolation coverage

The multi-client test creates two downstream stdio clients against the same shared router and then
invokes the same upstream tool concurrently with different inputs.

The assertion is simple and high value:

- client A sees only its own argument payload
- client B sees only its own argument payload

This is the cheapest strong proof that shared-upstream operation does not cross-wire responses.

## Investigation Notes

Two test-design details mattered:

### Mock server startup

Using a direct binary path was not reliable enough for this tranche because the binary was not
guaranteed to be prebuilt in every test context. Switching the integration helper to `cargo run`
made the end-to-end tests use the same reliable harness pattern already proven by the timeout /
reconnect test.

### Synchronization simplicity

The first multi-client version tried to synchronize the calls with a `Notify` gate. That added test
complexity without increasing confidence and risked hangs if the notification ordering drifted.

The simpler and better version was just:

- spawn both calls concurrently
- await both results
- assert each caller got its own payload

For this property, explicit concurrency is enough; extra orchestration was noise.

## Verification

This tranche was verified with:

- focused integration tests for:
  - stdio end-to-end proxy flow
  - HTTP end-to-end proxy flow with SSE
  - multi-client shared-engine isolation
- full suite:
  - `cargo test`
  - `cargo clippy --all-targets --all-features -- -D warnings`

## Prevention / Reuse

For future test work in `plug`:

- prefer real transport boundaries over direct router calls when validating user-facing behavior
- reuse the existing mock harness instead of creating new ad hoc fake servers
- if a synchronization primitive is not testing a real product property, remove it
- keep one layer of “true end-to-end” coverage for each transport, even when subsystem tests are
  strong

The durable lesson is that subsystem confidence and end-to-end confidence are different assets. The
former made Phase 1-3 implementation fast; this tranche adds the latter so future refactors are
safer.
