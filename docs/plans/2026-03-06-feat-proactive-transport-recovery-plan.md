---
title: "feat: Proactive Transport Recovery"
type: feat
status: active
date: 2026-03-06
origin: docs/brainstorms/2026-03-04-daemon-sharing-and-project-audit-brainstorm.md
---

# feat: Proactive Transport Recovery

## Overview

Make plug recover **before** the next tool call discovers a broken connection.

Today plug has the right recovery primitives, but they are mostly reactive:

- downstream `plug connect -> daemon` continuity repairs itself only when a request hits a dead IPC connection
- upstream `plug -> MCP server` recovery relies on one request-triggered reconnect attempt and slower health-check-driven recovery

That is materially better than manual `/mcp reconnect`, but it is not yet the product behavior the repo promises: silent, seamless, boring reliability.

This plan adds proactive recovery on both boundaries while preserving the conservative “never blindly replay ambiguous mutating calls” rule that was just implemented.

It carries forward the approved daemon-sharing decisions from [docs/brainstorms/2026-03-04-daemon-sharing-and-project-audit-brainstorm.md](/Users/robdezendorf/Documents/GitHub/plug/docs/brainstorms/2026-03-04-daemon-sharing-and-project-audit-brainstorm.md): tmux-style daemon lifecycle, shared daemon runtime, zero manual start steps, and fixing Rob’s real multi-client workflow first.

## Problem Statement / Motivation

The current behavior still allows a user-visible failed tool call during restart windows:

- If the daemon dies, `IpcProxyHandler` reconnects on demand and returns `REQUEST_RETRY_UNSAFE` for `tools/call` because the recovery only begins once a tool request arrives.
- If an upstream server restarts, `ToolRouter` does one immediate reconnect-and-retry on the failing call, while proactive recovery depends on coarse health polling. A fast restart window can still beat that first retry.

In practice this means:

- the LLM still sometimes sees one failed call before the system heals
- the assistant then has to decide whether to retry, which leaks transport plumbing into the user workflow
- the experience is reliable enough for manual use, but still too reactive for unattended agents and future remote HTTP clients

The next feature direction is downstream Streamable HTTP and remote clients. That makes this the right time to strengthen the continuity model instead of shipping the same reactive behavior on a second transport.

## Research Summary

### Origin Brainstorm

Found and used as the foundation:

- [docs/brainstorms/2026-03-04-daemon-sharing-and-project-audit-brainstorm.md](/Users/robdezendorf/Documents/GitHub/plug/docs/brainstorms/2026-03-04-daemon-sharing-and-project-audit-brainstorm.md)

Key carried-forward decisions:

- the daemon remains the authoritative shared runtime
- `plug connect` remains a thin transport adapter over daemon state
- the product should optimize for many concurrent local clients now, with a path to broader transport support later
- recovery should remove manual user steps rather than documenting more repair actions

### Existing Local Plans

Relevant existing work already in the repo:

- [docs/plans/2026-03-05-feat-daemon-client-session-continuity-plan.md](/Users/robdezendorf/Documents/GitHub/plug/docs/plans/2026-03-05-feat-daemon-client-session-continuity-plan.md)
  Established the logical `client_id` vs transport `session_id` model and reactive downstream continuity.
- [docs/plans/2026-03-04-feat-http-upstream-session-recovery-plan.md](/Users/robdezendorf/Documents/GitHub/plug/docs/plans/2026-03-04-feat-http-upstream-session-recovery-plan.md)
  Established reactive upstream reconnect and health-triggered proactive recovery.
- [docs/plans/2026-03-06-feat-streamable-http-client-continuity-boundary-plan.md](/Users/robdezendorf/Documents/GitHub/plug/docs/plans/2026-03-06-feat-streamable-http-client-continuity-boundary-plan.md)
  Explicitly preserved the continuity model as the right foundation for fast-follow Streamable HTTP clients.

### Repo Research

Relevant current implementation references:

- [plug/src/ipc_proxy.rs](/Users/robdezendorf/Documents/GitHub/plug/plug/src/ipc_proxy.rs)
  Downstream reconnect is request-triggered only. There is no background liveness or eager reattach.
- [plug/src/runtime.rs](/Users/robdezendorf/Documents/GitHub/plug/plug/src/runtime.rs)
  The daemon attach path already has the pieces needed for reconnect and auto-start.
- [plug-core/src/proxy/mod.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/proxy/mod.rs)
  Upstream `tools/call` does one reactive reconnect-and-retry on classified session/transport errors.
- [plug-core/src/health.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/health.rs)
  Proactive upstream recovery exists, but it is tied to health transitions and health checks are interval-based.
- [plug-core/src/engine.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/engine.rs)
  `Engine::reconnect_server()` already exists as the single reconnect codepath.
- [plug-core/src/server/mod.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/server/mod.rs)
  Upstream startup uses `start_server()` and currently relies on its normal initialize/list-tools path rather than a post-restart readiness probe loop.

### Institutional Learnings

Relevant learnings from `docs/solutions/`:

- [docs/solutions/integration-issues/mcp-multiplexer-http-transport-phase2.md](/Users/robdezendorf/Documents/GitHub/plug/docs/solutions/integration-issues/mcp-multiplexer-http-transport-phase2.md)
  Transport-specific lifecycle handling should stay at the edge while shared routing stays transport-agnostic.
- [docs/solutions/integration-issues/phase4-tui-dashboard-daemon-patterns.md](/Users/robdezendorf/Documents/GitHub/plug/docs/solutions/integration-issues/phase4-tui-dashboard-daemon-patterns.md)
  Keep daemon protocols explicit and boring. Prefer clear state transitions over magic retries hidden across layers.

## Proposed Solution

Implement proactive recovery in two focused layers:

1. **Proactive downstream daemon recovery**
   Add a background liveness/reconnect loop for `plug connect` so daemon restarts are usually repaired before the next MCP request arrives.

2. **Faster upstream restart readiness recovery**
   Tighten the upstream reconnect path so recent restarts are absorbed faster than the current “first call retries once, health checks recover later” behavior.

The recovery contract stays conservative:

- proactive reconnect is allowed
- background health/liveness state is allowed
- automatic replay of ambiguous `tools/call` requests is still **not** allowed

## Technical Approach

### A. Downstream State Machine

Add explicit connection state for the daemon-backed stdio adapter:

- `Healthy`
- `Reconnecting`
- `Failed`

Desired behavior:

- healthy path remains a normal mutex-guarded round trip
- a background task notices socket death or keepalive failure and begins reconnecting immediately
- while reconnecting, safe requests can wait briefly for repair rather than failing instantly
- ambiguous `tools/call` requests still return `REQUEST_RETRY_UNSAFE` if they raced the break before recovery completed

### B. Downstream Liveness Mechanism

Add a lightweight daemon liveness loop inside the daemon-backed connection path.

Options considered:

1. Passive read/write failure only
   Rejected: this is the current behavior and still leaks one failed tool call.
2. Dedicated heartbeat IPC message
   Best fit now.
3. OS-level socket polling only
   Rejected: too transport-specific and weaker than an explicit protocol-level liveness check.

Recommended implementation:

- add a cheap `Ping`/`Pong` IPC request pair
- spawn one background task per `IpcProxyHandler`
- ping on a short interval only while the stdio session is alive
- if ping fails with reconnectable transport error, reconnect immediately using existing runtime helpers
- update shared connection state atomically once the new session is registered and client info is restored

This keeps the daemon protocol explicit and reusable for future transport adapters.

### C. Request Coordination During Downstream Reconnect

Avoid reconnect stampedes and response cross-talk:

- only one reconnect attempt may run at a time per `plug connect` process
- in-flight request/response pairing remains protected by the existing connection mutex
- background reconnect and foreground round trips must share one coherent session snapshot

Recommended shape:

- keep the round-trip mutex
- add a `Notify` or watch-based reconnect coordinator alongside the mutex
- let foreground operations either:
  - use the healthy session immediately
  - wait briefly if a reconnect is already in progress
  - or return the existing explicit unsafe-retry error for ambiguous mutating calls

### D. Upstream Restart Readiness Recovery

The current upstream code already has:

- request-triggered reconnect in `ToolRouter`
- health-triggered proactive recovery in `health.rs`
- single-flight reconnect in `Engine::reconnect_server()`

The missing behavior is not “add proactive recovery from scratch.” It is:

- faster detection of recent restart windows
- bounded readiness/backoff close to the reconnect path

Recommended change:

- keep `Engine::reconnect_server()` as the single reconnect codepath
- add bounded retry/backoff around `do_reconnect()` for restart-like transport failures
- keep the retry window short and startup-focused, for example 100ms → 2s over a handful of attempts
- only use this readiness loop inside reconnect flows, not for every normal startup

This targets the real failure mode from the transcript: the first reconnect attempt happens before the upstream HTTP server is actually ready to accept requests.

### E. Transport-Neutral Continuity Boundary

Preserve the existing continuity boundary because Streamable HTTP is the fast-follow client transport:

- `client_id` remains the stable logical client identity
- `session_id` remains the attachment identity for the current transport session
- transport-specific liveness and reconnect logic remains local to the transport adapter

This plan should not invent a generic cross-transport resume subsystem yet. It should strengthen the boundary that future HTTP work will reuse.

## Alternative Approaches Considered

### 1. Keep everything reactive

Rejected.

This leaves one failed tool call as part of the normal user experience, which is exactly the product gap the user called out.

### 2. Blindly auto-replay all failed `tools/call` requests

Rejected.

That risks duplicated side effects after ambiguous writes, which is worse than one explicit retry signal.

### 3. Build a full generic continuity framework for all transports now

Rejected.

The second downstream transport does not exist yet. The right move is to harden the current seams, not freeze a premature abstraction.

## System-Wide Impact

- **Interaction graph**: stdio client liveness becomes partly background-driven instead of being discovered only by `tools/list` and `tools/call`.
- **Error propagation**: fewer transport errors should surface to the client, but the unsafe replay boundary remains explicit for ambiguous mutating calls.
- **State lifecycle risks**: reconnect state must be updated atomically so requests never mix old `session_id`, old `client_info`, and new transport halves.
- **API surface parity**: the new liveness contract should be reusable by future Streamable HTTP client attachments, even if the code is transport-specific today.
- **Integration test scenarios**:
  - daemon restarted while client is idle, next `tools/list` succeeds without visible reconnect error
  - daemon restarted while client is idle, next `tools/call` succeeds if recovery completed first
  - daemon restarted mid-request, `tools/call` still returns the conservative unsafe-retry error
  - upstream HTTP server restarted, first post-restart tool call succeeds after bounded reconnect readiness wait
  - multiple concurrent clients recover without reconnect stampede or identity collision

## Implementation Phases

### Phase 1: Plan and Protocol

- add explicit IPC `Ping` / `Pong`
- document downstream reconnect state transitions
- keep the public continuity model (`client_id` vs `session_id`) unchanged

### Phase 2: Proactive Downstream Recovery

- add background daemon liveness task in the daemon-backed client path
- add reconnect coordination so one reconnect repairs shared state for all foreground operations
- preserve current unsafe replay handling for ambiguous `tools/call`

### Phase 3: Upstream Readiness Tightening

- add bounded backoff/readiness behavior inside upstream reconnect flows
- keep `Engine::reconnect_server()` as the single reconnect entry point
- ensure proactive health recovery and reactive request recovery share the same semantics

### Phase 4: Tests and Evidence

- unit tests for new IPC request types and downstream reconnect coordination
- integration-style tests for daemon restart while connected
- integration-style tests for upstream HTTP restart/readiness windows
- manual repro against `plug stop` / `plug start` and an HTTP upstream restart

## Acceptance Criteria

- [ ] Idle daemon restarts are repaired proactively so the next `tools/list` does not need to trigger reconnect itself
- [ ] Idle daemon restarts are usually repaired before the next `tools/call`, reducing or eliminating visible `REQUEST_RETRY_UNSAFE` for restart windows where recovery completes in time
- [ ] Ambiguous mutating tool calls still do **not** get blindly replayed
- [ ] Upstream restart recovery no longer relies on one immediate reconnect attempt alone; bounded readiness/backoff exists in the reconnect path
- [ ] Restarting a local HTTP upstream server is usually absorbed without a visible failed first tool call once the bounded readiness loop is in place
- [ ] Multiple concurrent local clients can recover independently without reconnect collisions
- [ ] The resulting design still preserves the logical `client_id` vs transport `session_id` boundary needed for downstream Streamable HTTP work

## Success Metrics

- fewer or no user-visible failed tool calls during routine daemon restarts
- fewer or no user-visible failed first calls during routine upstream restart windows
- no regression in unsafe replay correctness for `tools/call`
- future Streamable HTTP client work can reuse the same continuity model without first undoing this design

## Dependencies & Risks

### Dependencies

- [docs/plans/2026-03-05-feat-daemon-client-session-continuity-plan.md](/Users/robdezendorf/Documents/GitHub/plug/docs/plans/2026-03-05-feat-daemon-client-session-continuity-plan.md)
- [docs/plans/2026-03-04-feat-http-upstream-session-recovery-plan.md](/Users/robdezendorf/Documents/GitHub/plug/docs/plans/2026-03-04-feat-http-upstream-session-recovery-plan.md)

### Risks

- adding background reconnect without careful coordination could create torn connection state
- overly aggressive liveness traffic could add unnecessary daemon chatter
- overly broad upstream readiness retries could hide real misconfiguration instead of fast restart windows
- premature abstraction could still creep in if the implementation tries to solve remote HTTP resume before that feature exists

## Recommended Next Step

Implement the smallest version that changes the user experience materially:

1. add explicit daemon `Ping` / `Pong`
2. add background downstream liveness + reconnect coordination
3. add short bounded readiness/backoff to upstream reconnect
4. prove the behavior with restart-focused tests before widening the scope further

## Sources & References

- **Origin brainstorm:** [docs/brainstorms/2026-03-04-daemon-sharing-and-project-audit-brainstorm.md](/Users/robdezendorf/Documents/GitHub/plug/docs/brainstorms/2026-03-04-daemon-sharing-and-project-audit-brainstorm.md)
- [docs/plans/2026-03-05-feat-daemon-client-session-continuity-plan.md](/Users/robdezendorf/Documents/GitHub/plug/docs/plans/2026-03-05-feat-daemon-client-session-continuity-plan.md)
- [docs/plans/2026-03-04-feat-http-upstream-session-recovery-plan.md](/Users/robdezendorf/Documents/GitHub/plug/docs/plans/2026-03-04-feat-http-upstream-session-recovery-plan.md)
- [docs/plans/2026-03-06-feat-streamable-http-client-continuity-boundary-plan.md](/Users/robdezendorf/Documents/GitHub/plug/docs/plans/2026-03-06-feat-streamable-http-client-continuity-boundary-plan.md)
- [plug/src/ipc_proxy.rs](/Users/robdezendorf/Documents/GitHub/plug/plug/src/ipc_proxy.rs)
- [plug/src/runtime.rs](/Users/robdezendorf/Documents/GitHub/plug/plug/src/runtime.rs)
- [plug-core/src/proxy/mod.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/proxy/mod.rs)
- [plug-core/src/health.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/health.rs)
- [plug-core/src/engine.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/engine.rs)
