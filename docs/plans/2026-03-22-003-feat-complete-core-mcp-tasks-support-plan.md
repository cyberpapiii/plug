---
title: Complete Core MCP Tasks Support
type: feat
status: active
date: 2026-03-22
---

# Complete Core MCP Tasks Support

## Overview

`plug` is already strong across the core MCP surface: tools, resources, resource templates, prompts, completions, logging, progress, cancellation, roots, sampling, and elicitation are all implemented and routed across stdio, HTTP, and daemon IPC. The largest remaining protocol-completeness gap on `main` is **full Tasks support**.

Today, `plug` preserves and enriches `Tool.execution.taskSupport`, but it does not yet implement the Tasks utility protocol itself. That leaves a mismatch between what the latest MCP spec allows clients and servers to negotiate and what `plug` can actually route or terminate. The result is that `plug` is metadata-aware for tasks, but not task-protocol aware.

This plan closes that gap first, then tightens adjacent completeness items around auth/discovery metadata and protocol-surface auditing.

## Scope Boundary

This plan is intentionally split into:

- **Tranche 1:** complete core MCP Tasks support for tool calls
- **Follow-on:** auth/discovery completeness items discovered during the same audit

Tranche 1 is the implementation priority and should be executable without waiting on broader auth/discovery cleanup.

## Problem Statement / Motivation

The latest MCP spec at `2025-11-25` includes Tasks as a first-class utility surface. Tasks are not just a tool hint; they define:

- capability negotiation via `capabilities.tasks`
- task-augmented requests
- task lifecycle tracking
- task retrieval, listing, and cancellation
- task status transitions and deferred result retrieval

`plug` currently supports only the hint layer:

- tool `execution.taskSupport` is preserved and enriched
- no synthesized `tasks` capability exists
- no `tasks/*` methods are routed or terminated
- no task state model exists inside `plug`

This means `plug` is still incomplete against the latest core protocol, even though much of the rest of the server/client/utilities surface is already present.

## Proposed Solution

Implement a **Tasks-first core protocol completion pass** for `plug`, with the following scope:

1. Add full downstream/server-side Tasks capability support.
2. Add task-aware request routing and lifecycle management across stdio, HTTP, and daemon IPC.
3. Preserve and route upstream task-capable servers correctly when those surfaces are present.
4. Add a method-by-method MCP compliance matrix for the 2025-11-25 core spec.

The design should treat tasks as a cross-transport, cross-direction protocol feature, not as a tool-only annotation.

## Technical Approach

### Architecture

Introduce a `TaskRouter` / `TaskStore` layer owned alongside `ToolRouter` and used by all downstream transports.

Core responsibilities:

- accept task-augmented downstream requests
- decide whether `plug` is acting as:
  - a pass-through task-aware router to upstream support
  - or a task-terminating wrapper around a normal upstream request
- persist task lifecycle state for polling and result retrieval
- route task notifications and task cancellation correctly
- keep task identity stable across daemon IPC reconnects and downstream HTTP sessions

Suggested modules:

- `plug-core/src/tasks/mod.rs`
- `plug-core/src/tasks/store.rs`
- `plug-core/src/tasks/types.rs`
- `plug-core/src/tasks/router.rs`

Suggested integration points:

- [plug-core/src/proxy/mod.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/proxy/mod.rs)
- [plug-core/src/http/server.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/http/server.rs)
- [plug/src/daemon.rs](/Users/robdezendorf/Documents/GitHub/plug/plug/src/daemon.rs)
- [plug/src/ipc_proxy.rs](/Users/robdezendorf/Documents/GitHub/plug/plug/src/ipc_proxy.rs)

### Protocol Surface To Add

At minimum, align `plug` with the Tasks utility defined in the spec:

- `capabilities.tasks`
- task-augmented request parsing/forwarding
- task creation
- task retrieval
- task result retrieval
- task listing
- task cancellation
- task status notifications / lifecycle handling

If upstream servers do not support Tasks, `plug` should still be able to expose a coherent downstream task experience by wrapping long-running requests in `plug`-owned task state when appropriate.

### Capability Synthesis

Update capability synthesis so `plug` advertises `tasks` only when the runtime can actually honor it.

That means:

- no “metadata-only” task support
- no advertising `tasks` just because some tools contain `execution.taskSupport`
- per-transport masking remains honest

This belongs in:

- [plug-core/src/proxy/mod.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/proxy/mod.rs)

### Task Ownership Model

Use a two-mode strategy:

1. **Pass-through mode**
   If an upstream server natively supports task-augmented requests and `plug` can preserve task semantics without semantic loss, pass through and map downstream task IDs/state to upstream task IDs/state.

2. **Wrapper mode**
   If upstream does not support Tasks, `plug` creates and owns task state itself, wrapping the underlying request and exposing spec-compliant polling/result retrieval to the downstream client.

This keeps compatibility broad while still improving protocol completeness for clients.

### Task Identity, Retention, And Garbage Collection

Tasks must be modeled as durable protocol state, not as request-local bookkeeping.

Required properties:

- downstream-visible task IDs that outlive the originating request
- explicit owner binding to the downstream authenticated client/session identity
- optional upstream task ID mapping in pass-through mode
- terminal result retention policy
- garbage collection / TTL policy

For the first tranche, the recommended policy is:

- retain task state across config reload
- do not promise retention across daemon restart yet
- keep completed/failed/cancelled task results available for a bounded retention window
- document task expiry semantics explicitly

This avoids overcommitting to persistence before the runtime model is proven, while still giving polling clients a real task lifecycle.

### Task Capability Honesty Per Transport

`plug` should only advertise `capabilities.tasks` on transports that can actually honor:

- task creation
- task retrieval/listing
- task result retrieval
- task cancellation

This means:

- no capability advertisement based only on `Tool.execution.taskSupport`
- no transport claiming Tasks support if retrieval/cancel flows are missing there
- transport masking must remain as honest for Tasks as it already is for resources, prompts, completions, and logging

### Request Categories To Support First

Tasks should first be supported for:

- tool calls
- reverse requests with long-running semantics where appropriate

The first implementation does not need to overgeneralize immediately to every possible request shape. It should focus on the real requests `plug` already routes and where task semantics are meaningful.

Explicit non-goal for tranche 1:

- no generalized task support for every possible MCP request category
- start with tool calls first, then expand only after the core lifecycle is stable

### Explicit Non-Goals For Tranche 1

The following are intentionally out of scope for the first implementation:

- generalized Tasks support for all MCP request categories
- persistence across daemon restart
- full auth/discovery metadata cleanup
- extensions or MCP Apps work
- richer CLI inspection UX beyond what is needed for verification

### Cancellation Semantics

The spec is explicit that:

- plain `notifications/cancelled` is not the same as `tasks/cancel`
- task-augmented requests use task cancellation, not generic request cancellation

So the implementation must keep both paths separate:

- request cancellation for ordinary in-flight requests
- task cancellation for durable task state

Additional requirements carried forward from existing progress/cancel learnings:

- task-backed calls must preserve exact upstream request identity where relevant
- progress/correlation state must exist before upstream execution can emit callbacks
- progress/cancel/result routing must tolerate interleaving and out-of-order delivery

### Task Authorization And Ownership

Tasks introduce a new authorization boundary.

The implementation must explicitly define:

- who can read a task
- who can list tasks
- who can retrieve a task result
- who can cancel a task

For the initial implementation:

- task IDs and task state must be scoped to the downstream authenticated client/session identity
- only the owning client/session may retrieve or cancel a task
- cross-session task retrieval and cancellation must fail explicitly
- task/result access control must behave consistently across stdio, HTTP, and daemon IPC

Protected-resource metadata completeness is a follow-on concern, but task authorization is part of the first tranche and cannot be deferred.

### Persistence / Runtime Ownership

Task state must survive the same runtime ownership boundaries that already exist elsewhere:

- daemon-owned shared runtime
- reconnecting IPC proxy sessions
- downstream HTTP sessions

The daemon should be the source of truth for task state when running in background-service mode.

Architectural rule:

- `ToolRouter` remains immutable and routing-focused
- mutable task lifecycle state lives in a runtime-owned `TaskStore`, never in `RouterSnapshot`
- transports are adapters, not task-state owners

Runtime mode split:

- **daemon mode**: daemon-owned task authority; IPC and HTTP clients reference daemon-owned task IDs/state
- **standalone foreground serve mode**: in-process runtime owns task state locally

Additional runtime constraints from existing learnings:

- active task state must be shutdown-aware so daemon restart does not leave half-alive sessions hiding recovery bugs
- dead downstream HTTP reverse-request targets must fail fast rather than accumulating pending task-related state
- generation-based ownership or equivalent epoch semantics should be used where background task supervisors can overlap reload/replacement boundaries

### Auth / Discovery Follow-On

During the same completeness pass, audit and record:

- protected-resource auth metadata behavior
- well-known metadata consistency
- any capability/method mismatches discovered during the full matrix audit

This is explicitly follow-on work and should not delay tranche 1 unless a blocking remote-client incompatibility is discovered.

## Implementation Phases

### Phase 1: Spec Matrix And Design Baseline

Goals:

- freeze a method-by-method compliance matrix for the 2025-11-25 core MCP surface
- explicitly classify each area as `implemented`, `partial`, `missing`, or `intentionally unsupported`
- finalize the task ownership model and capability rules
- make the tranche-1 boundary explicit and enforceable

Tasks:

- create `docs/research/2026-03-22-core-mcp-completeness-matrix.md`
- enumerate:
  - lifecycle
  - transports
  - authorization/discovery
  - utilities
  - client features
  - server features
- mark current `plug` coverage with file references
- record exact Tasks protocol messages and capability shapes to implement
- include a checkpoint for protected-resource metadata completeness and remote-client discovery expectations

Success criteria:

- one canonical completeness matrix exists
- Tasks scope is explicit and bounded
- no ambiguity remains about what “done” means for core protocol completeness
- auth/discovery follow-on items are recorded separately from tranche-1 acceptance

### Phase 2: Task Capability And Types Foundation

Goals:

- add internal task types and store
- add capability synthesis support

Tasks:

- add internal task domain model
- add task store keyed by downstream-visible task IDs
- define task owner/runtime epoch fields
- define task retention and garbage collection policy
- add daemon IPC message additions for task methods
- update synthesized capabilities to advertise `tasks` only when supported
- add serde/model integration tests for task-related request/response shapes
- add a task state-machine test suite covering monotonic terminal transitions

Success criteria:

- `plug` can represent task state internally
- capabilities remain honest per transport

### Phase 3: Tool Task Wrapping And Lifecycle

Goals:

- support task-augmented tool calls end to end

Tasks:

- parse task-augmented tool requests
- implement wrapper-mode task execution around long-running tools
- register task/correlation state before any progress/status callback can arrive
- persist status transitions
- support retrieval of task state/results
- support task cancellation
- wire progress/status notifications
- add wrapper-mode stdio and HTTP happy-path end-to-end tests

Success criteria:

- a downstream client can create, poll, and cancel task-backed tool work through `plug`
- lifecycle is coherent across stdio, HTTP, and daemon IPC
- terminal task state remains retrievable after disconnect/reconnect within the retention window

### Phase 4: Upstream Pass-Through And Cross-Transport Parity

Goals:

- preserve upstream task semantics when available
- maintain parity across transports

Tasks:

- map upstream task state into downstream task state where possible
- normalize upstream task IDs, status vocabulary, cancellation semantics, and result retrieval shape before exposing them downstream
- route task operations across daemon IPC
- add transport parity tests
- verify reconnect/reload interactions with active tasks
- add shared-engine multi-client isolation tests
- add daemon continuity and reload-during-active-task tests

Success criteria:

- pass-through mode works where possible
- wrapper mode remains the fallback
- no transport-specific task correctness gaps remain
- stale or replaced IPC sessions cannot mutate active task state

### Phase 5: Auth / Discovery Completeness Follow-On

Goals:

- close any adjacent protocol-completeness gaps uncovered during the audit

Tasks:

- implement or explicitly scope the `/.well-known/oauth-protected-resource` story
- align auth/discovery metadata with latest spec expectations
- update tests for discovery/auth completeness

Success criteria:

- auth/discovery surface is coherent with the current spec and remote-client expectations
- phase 5 remains optional relative to tranche-1 Tasks completion unless a proven blocker emerges

## System-Wide Impact

### Interaction Graph

Task support will touch nearly every protocol-facing layer:

- downstream initialize -> capability synthesis
- downstream request parsing -> task-aware dispatch
- task store -> tool execution lifecycle
- daemon IPC -> task state ownership / reconnect parity
- downstream HTTP -> task polling / result retrieval
- reverse requests -> possible task augmentation interactions

Action chain example:

- client submits task-augmented tool call
- downstream transport parses task metadata
- `TaskRouter` creates durable task state
- underlying tool call is routed through `ToolRouter`
- progress/cancel notifications update task state
- client polls for task/result

### Error & Failure Propagation

Critical failure classes to design for:

- upstream tool failure after task creation
- daemon restart during active task
- downstream disconnect during task execution
- cancellation races
- duplicate task retrieval/cancel requests
- pass-through mode where upstream task semantics differ from local expectations
- unauthorized cross-session task retrieval/cancellation attempts
- dead downstream HTTP reverse-request targets during task-related flows

The system must preserve deterministic task states even under partial failure.

### State Lifecycle Risks

Tasks create durable protocol state. Risks include:

- orphaned tasks after runtime reload
- lost terminal results after reconnect
- task/result mismatch across pass-through and wrapper modes
- leaking long-running task state forever
- cross-session task/result leakage
- guessable or enumerable task identifiers

The design must include:

- TTL or retention policy
- terminal state cleanup policy
- explicit daemon ownership rules
- explicit owner binding for task access control

### API Surface Parity

Surfaces that must remain aligned:

- stdio (`plug connect`)
- HTTP/HTTPS (`plug serve`)
- daemon IPC proxy

The same task semantics must hold regardless of how the downstream client is connected.

### Integration Test Scenarios

- task-augmented tool call over stdio with progress and successful result retrieval
- task cancellation over HTTP with correct terminal state
- daemon IPC reconnect while a task is in progress
- upstream pass-through task mapping when an upstream server supports Tasks
- wrapper-mode task state for a long-running non-task-native upstream tool
- shared-engine multi-client task isolation
- reload during active task does not orphan or duplicate state
- task protocol responses over daemon IPC tolerate interleaved logging/control/task notifications safely

## Acceptance Criteria

### Functional Requirements

- [ ] `plug` synthesizes and advertises `tasks` capability only when actually supported
- [ ] task-augmented tool requests work end to end in tranche 1
- [ ] downstream clients can retrieve task state and final results
- [ ] downstream clients can cancel tasks using task-specific cancellation semantics
- [ ] wrapper-mode and pass-through-mode task execution are both supported where appropriate
- [ ] stdio, HTTP, and daemon IPC behavior are functionally consistent
- [ ] task retrieval and cancellation enforce owner/session access control
- [ ] no task ID cross-talk occurs across concurrent downstream clients
- [ ] task state is retained across config reload and reconnect within the same runtime lifetime
- [ ] the implementation does not advertise or imply retention across daemon restart

### Non-Functional Requirements

- [ ] capability synthesis remains honest and transport-aware
- [ ] task state survives daemon-backed runtime conditions predictably
- [ ] task lifecycle behavior is deterministic under cancellation and reconnect races
- [ ] no regression to existing progress/cancel behavior for non-task requests
- [ ] terminal states are monotonic and durable within the documented retention window
- [ ] dead downstream delivery targets fail fast without leaking pending task-related state

### Quality Gates

- [ ] `cargo test --workspace` passes
- [ ] targeted task integration coverage exists for stdio, HTTP, and daemon IPC
- [ ] compliance matrix is written and updated alongside implementation
- [ ] task ownership/authz negative tests exist for read/cancel/result access
- [ ] tranche-1 non-goals remain out of the implementation unless explicitly reopened

## Success Metrics

- `plug` moves from task-metadata-aware to task-protocol-aware
- the 2025-11-25 core MCP completeness matrix shows Tasks as implemented rather than partial
- remote and local clients can use long-running task semantics without custom `plug`-specific behavior
- active-task behavior remains truthful and stable across reload/reconnect scenarios

## Dependencies & Risks

### Dependencies

- current routing architecture in `ToolRouter`, daemon IPC, and HTTP server
- rmcp model support for task-related protocol types
- stable task capability semantics in the current spec revision

### Risks

- Tasks are marked experimental in the spec and may evolve
- wrapper-mode and pass-through-mode semantics may drift if overgeneralized too early
- daemon/runtime complexity could increase if task persistence is overbuilt

### Mitigations

- keep the first tranche focused on tool-task support
- keep task state model simple and explicit
- preserve transport parity through integration tests rather than ad hoc fixes
- treat the compliance matrix as the truth source for what is done vs partial
- prefer wrapper mode over leaky pass-through when upstream semantics do not normalize cleanly

## Alternative Approaches Considered

### 1. Skip Tasks And Jump To Extensions

Rejected because Tasks are core-spec functionality now, while extensions remain optional.

### 2. Treat `taskSupport` Metadata As Good Enough

Rejected because it advertises semantics without implementing the actual protocol behavior.

### 3. Implement A Giant Fully General Task Engine Immediately

Rejected because it adds too much complexity before proving the core tool-task path.

### 4. Couple Tasks Completion To Auth/Discovery Cleanup

Rejected because it turns one protocol-completeness gap into a broad multi-stream delivery target. Auth/discovery follow-on work should be recorded and executed separately unless it becomes a hard blocker.

## Sources & References

### Internal References

- [docs/PROJECT-STATE-SNAPSHOT.md](/Users/robdezendorf/Documents/GitHub/plug/docs/PROJECT-STATE-SNAPSHOT.md)
- [docs/PLAN.md](/Users/robdezendorf/Documents/GitHub/plug/docs/PLAN.md)
- [plug-core/src/proxy/mod.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/proxy/mod.rs)
- [plug-core/src/http/server.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/http/server.rs)
- [plug/src/daemon.rs](/Users/robdezendorf/Documents/GitHub/plug/plug/src/daemon.rs)
- [plug/src/ipc_proxy.rs](/Users/robdezendorf/Documents/GitHub/plug/plug/src/ipc_proxy.rs)
- [plug-core/src/enrichment.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/enrichment.rs)
- [docs/solutions/integration-issues/phase2b-progress-cancellation-routing-20260307.md](/Users/robdezendorf/Documents/GitHub/plug/docs/solutions/integration-issues/phase2b-progress-cancellation-routing-20260307.md)
- [docs/solutions/integration-issues/completion-pass-through-forwarding-20260307.md](/Users/robdezendorf/Documents/GitHub/plug/docs/solutions/integration-issues/completion-pass-through-forwarding-20260307.md)
- [docs/solutions/integration-issues/2026-03-18-ipc-interleaving-buffering.md](/Users/robdezendorf/Documents/GitHub/plug/docs/solutions/integration-issues/2026-03-18-ipc-interleaving-buffering.md)
- [docs/solutions/integration-issues/phase3c-daemon-continuity-recovery-20260307.md](/Users/robdezendorf/Documents/GitHub/plug/docs/solutions/integration-issues/phase3c-daemon-continuity-recovery-20260307.md)
- [docs/solutions/integration-issues/2026-03-18-http-reverse-request-fail-fast.md](/Users/robdezendorf/Documents/GitHub/plug/docs/solutions/integration-issues/2026-03-18-http-reverse-request-fail-fast.md)
- [docs/solutions/integration-issues/2026-03-18-reload-topology-background-tasks.md](/Users/robdezendorf/Documents/GitHub/plug/docs/solutions/integration-issues/2026-03-18-reload-topology-background-tasks.md)
- [docs/solutions/integration-issues/2026-03-18-reload-health-refresh-coalescing.md](/Users/robdezendorf/Documents/GitHub/plug/docs/solutions/integration-issues/2026-03-18-reload-health-refresh-coalescing.md)
- [docs/solutions/integration-issues/phase3b-e2e-integration-test-foundation-20260307.md](/Users/robdezendorf/Documents/GitHub/plug/docs/solutions/integration-issues/phase3b-e2e-integration-test-foundation-20260307.md)

### External References

- MCP Spec 2025-11-25: https://modelcontextprotocol.io/specification/2025-11-25
- Client Features: https://modelcontextprotocol.io/specification/2025-11-25/client
- Server Features: https://modelcontextprotocol.io/specification/2025-11-25/server
- Utilities: https://modelcontextprotocol.io/specification/2025-11-25/basic/utilities
- Tasks: https://modelcontextprotocol.io/specification/2025-11-25/basic/utilities/tasks
- Extensions Overview: https://modelcontextprotocol.io/extensions/overview
