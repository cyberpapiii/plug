---
title: "feat: phase 3b end-to-end integration tests"
type: feat
status: completed
date: 2026-03-07
origin: docs/brainstorms/2026-03-07-phase3b-e2e-integration-tests-brainstorm.md
---

# Phase 3B End-to-End Integration Tests

## Overview

Add the first real end-to-end integration-test foundation for `plug`, covering the full proxy path
through stdio and HTTP plus shared-upstream multi-client behavior.

## Problem Statement / Motivation

The codebase now has strong component and subsystem tests:

- router behavior
- notifications
- progress/cancellation
- HTTP session/SSE plumbing
- meta-tool mode

What is still missing is strong proof that `plug` behaves correctly across its real boundaries when
driven like a client drives it. The current `plug-core/tests/integration_tests.rs` file is still
mostly configuration and low-level runtime verification.

The next quality step is to convert that into actual end-to-end proxy-path confidence.

## Proposed Solution

Build a narrow integration-test foundation that uses the existing `plug-test-harness` mock server
and real `Engine`/transport wiring to verify:

1. stdio end-to-end proxy path
2. HTTP end-to-end proxy path with SSE stream attached
3. multiple downstream clients sharing one engine without cross-contamination

This deliberately excludes daemon continuity and upstream crash/restart choreography for now.

## Technical Considerations

- Reuse `plug-test-harness` instead of inventing a second mock-server mechanism
- Keep the new tests in `plug-core/tests/integration_tests.rs` unless a split file is clearly
  warranted
- Prefer full path tests with real request ordering over deeper mocking
- Avoid fragile sleep-heavy timing when synchronization can be explicit

## System-Wide Impact

- **Interaction graph**: downstream client request -> transport handler -> `ToolRouter` ->
  `ServerManager` -> real mock upstream -> response routed back to caller.
- **Error propagation**: failures should surface through the same error paths as production code,
  not test-only wrappers.
- **State lifecycle risks**: multi-client tests must ensure active call tracking and response
  routing do not leak across clients.
- **API surface parity**: stdio and HTTP must each be exercised through their real downstream entry
  points.
- **Integration test scenarios**:
  - stdio initialize -> tools/list -> tools/call
  - HTTP initialize -> SSE attach -> tools/list -> tools/call
  - two downstream clients sharing one upstream server, each receiving only its own response

## Acceptance Criteria

- [x] Add a real stdio end-to-end integration test using the mock MCP server
- [x] Add a real HTTP end-to-end integration test that exercises initialize, SSE, and tool call
- [x] Add a multi-client shared-engine integration test proving no response cross-contamination
- [x] Keep tests deterministic enough to pass under the normal suite without flaky retries
- [x] Full suite passes with the new tests in place

## Success Metrics

- `plug-core/tests/integration_tests.rs` now contains real transport-level end-to-end coverage
- regressions in proxy boundary behavior are caught without needing manual smoke validation

## Dependencies & Risks

- The current mock test server only supports stdio tool calls, so HTTP tests may need to use the
  real `HttpState`/Axum stack on top of the existing engine rather than a separate mock transport
- Multi-client tests can become flaky if synchronization relies on arbitrary sleeps rather than
  observable boundaries
- This tranche must avoid growing into daemon continuity or restart choreography work

## Sources & References

- **Origin brainstorm:** `docs/brainstorms/2026-03-07-phase3b-e2e-integration-tests-brainstorm.md`
- `docs/plans/2026-03-06-feat-strategic-stabilize-comply-compete-plan.md`
- `plug-core/tests/integration_tests.rs`
- `plug-test-harness/src/bin/mock-server.rs`
- `plug-core/src/http/server.rs`
- `plug-core/src/server/mod.rs`
- `docs/solutions/integration-issues/phase2a-notification-infrastructure-tools-list-changed-20260307.md`
- `docs/solutions/integration-issues/phase2b-progress-cancellation-routing-20260307.md`
