# Brainstorm: Phase 3B End-to-End Integration Test Foundation

**Date**: 2026-03-07
**Status**: Ready for planning

## What We're Building

The next Phase 3 tranche is a real end-to-end integration test foundation that exercises `plug`
through full request paths instead of mostly router-level or component-level tests.

This tranche focuses on:

- stdio end-to-end proxy flow with a real upstream test server
- HTTP end-to-end proxy flow including initialize, SSE attachment, and tool invocation
- multi-client shared-upstream behavior to prove client isolation on a shared engine

This tranche does **not** include:

- daemon restart continuity
- upstream kill/restart recovery
- stateless session abstraction/design
- large documentation cleanup beyond the new test artifacts

## Why This Approach

The roadmap item for “rmcp 1.0.0 → 1.1.0” is already stale in practice: the workspace is already
resolving `rmcp v1.1.0`. The meaningful remaining gap is confidence at the system boundary.

Today’s test suite is strong at the router, session, notification, and targeted transport layers.
What is still weak is proof that the full proxy path behaves correctly when driven the way real
clients drive it:

- initialize
- list tools
- call tools
- attach SSE where applicable
- share one engine across multiple downstream clients

That makes end-to-end integration coverage the highest-value next tranche.

## Key Decisions

- **Treat this as a narrow testing tranche, not a grab-bag Phase 3 pass.**
  The goal is confidence, not clearing every remaining roadmap bullet at once.

- **Prefer tests that cross real boundaries.**
  Use the existing `plug-test-harness` mock server and real runtime/transport wiring instead of
  re-mocking the router in new ways.

- **Cover both transports through the actual proxy surface.**
  The same shared router now powers stdio and HTTP; the tests should prove parity at that level.

- **Add multi-client coverage before daemon continuity.**
  Shared-upstream isolation is core product behavior and cheaper to verify now than full daemon
  continuity orchestration.

- **Defer daemon continuity and crash/restart choreography.**
  Those are valid next steps, but they require more harness complexity and should come after the
  basic end-to-end foundation exists.

## Resolved Questions

- **Should the next tranche be the rmcp upgrade?** No. The build graph already resolves to
  `rmcp v1.1.0`.
- **Should this tranche include daemon continuity?** No.
- **Should this tranche include upstream crash/restart recovery?** No.
- **Should this tranche focus on real end-to-end paths instead of more unit tests?** Yes.

## Open Questions

None. The scope is intentionally narrow enough to proceed directly to planning.

## Next

Write a focused plan for:

1. stdio end-to-end test path
2. HTTP end-to-end test path with SSE attached
3. multi-client isolation on a shared engine
4. quality gate updates and any small harness extensions required
