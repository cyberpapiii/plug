---
title: "feat: phase 2b progress and cancellation routing"
type: feat
status: completed
date: 2026-03-07
origin: docs/brainstorms/2026-03-07-phase2b-progress-cancellation-routing-brainstorm.md
---

# Phase 2B Progress and Cancellation Routing

## Overview

Build the request-scoped control path that sits on top of Phase 2A: preserve downstream progress tokens, forward downstream cancellation to the correct upstream request, and relay upstream progress notifications back to the correct downstream stdio or HTTP client.

## Problem Statement / Motivation

Phase 2A added global upstream notification handling and downstream fan-out for `tools/list_changed`, but the request-scoped half of MCP control flow is still missing:

- downstream `notifications/cancelled` are validated and dropped
- upstream `notifications/progress` can now be received by `rmcp` but are not routed anywhere
- `ToolRouter` tracks active downstream request identity, but not enough metadata to route progress or cancellation end to end
- HTTP and stdio both have notification delivery paths, but only for broadcast-style list-change notifications

Without this tranche, long-running tools cannot be cancelled correctly and progress remains invisible even though the notification infrastructure now exists.

## Proposed Solution

### Scope

This phase includes:

- expand the active-call correlation layer to track downstream request ID, session identity, upstream request identity, and optional progress token
- handle downstream `notifications/cancelled` and forward them to the appropriate upstream peer
- preserve downstream `progressToken` when present and pass it upstream on `tools/call`
- route upstream `notifications/progress` back to the correct downstream stdio or HTTP client

This phase excludes:

- resources/prompts forwarding
- pagination
- full capability synthesis
- any synthetic progress token generation for clients that do not supply one

### Technical Approach

1. **Active call record expansion**
   Replace the current minimal `DownstreamCallContext` retention with an explicit active-call record that stores:
   - downstream transport
   - downstream request ID
   - optional downstream session ID
   - upstream server ID
   - upstream request ID / internal call ID
   - optional progress token

2. **Cancellation forwarding**
   Handle `notifications/cancelled` in both downstream entry points:
   - stdio `ProxyHandler`
   - HTTP `post_mcp`

   Resolve the active call by downstream request identity and forward `notify_cancelled(...)` to the upstream peer. If the call has already completed, drop silently.

3. **Progress token passthrough**
   When a downstream `tools/call` includes `_meta.progressToken`, preserve it on the upstream request using rmcp’s `RequestParamsMeta` support and store the mapping in the active-call record.

4. **Upstream progress relay**
   Extend the upstream client handler to receive `notifications/progress`, resolve the active call via progress token, and route the notification to the correct downstream transport:
   - stdio via stored `Peer<RoleServer>`
   - HTTP via session-targeted SSE send

5. **Targeted protocol notification path**
   Keep `tools/list_changed` on the global protocol-notification bus. Add a targeted delivery path for per-request progress/cancelled semantics rather than rebroadcasting those notifications to all clients.

## System-Wide Impact

- **Interaction graph**
  downstream `tools/call` -> active-call record creation -> optional progress token preservation -> upstream call
  downstream `notifications/cancelled` -> active-call lookup -> upstream `notify_cancelled`
  upstream `notifications/progress` -> active-call lookup -> targeted downstream delivery

- **Error propagation**
  Missing active-call mappings should be logged at debug/warn level and dropped without poisoning unrelated clients or breaking the engine.

- **State lifecycle risks**
  Active-call records must be cleaned up on success, error, timeout, reconnect failure, and session teardown.

- **API surface parity**
  Both stdio and HTTP must be able to:
  - send cancellation downstream -> upstream
  - receive targeted progress updates upstream -> downstream

- **Integration test scenarios**
  - stdio client sends `notifications/cancelled` and upstream receives it
  - HTTP client sends `notifications/cancelled` and upstream receives it
  - stdio `tools/call` with `progressToken` receives upstream progress
  - HTTP `tools/call` with `progressToken` receives upstream progress over SSE
  - completed calls are removed from routing maps and no longer receive progress/cancel forwarding

## Acceptance Criteria

- [x] Downstream `notifications/cancelled` are no longer dropped silently
- [x] Cancellation is forwarded to the correct upstream request for both stdio and HTTP downstreams
- [x] Downstream `progressToken` is preserved on upstream `tools/call` requests
- [x] Upstream `notifications/progress` are routed to the correct downstream stdio or HTTP client
- [x] Active-call records are cleaned up after completion/failure
- [x] Focused tests cover cancellation and progress for both transports

## Dependencies & Risks

- This tranche depends directly on the Phase 2A notification substrate being present and merged
- The correlation layer must stay minimal but coherent; avoid over-designing a general task/session abstraction here
- HTTP targeted progress delivery must not regress the Phase 2A hardening around slow/expired SSE clients

## Sources & References

- **Origin brainstorm:** `docs/brainstorms/2026-03-07-phase2b-progress-cancellation-routing-brainstorm.md`
- `docs/plans/2026-03-06-feat-strategic-stabilize-comply-compete-plan.md`
- `docs/plans/2026-03-07-feat-phase2a-notification-infrastructure-plan.md`
- `docs/solutions/integration-issues/phase2a-notification-infrastructure-tools-list-changed-20260307.md`
- `plug-core/src/proxy/mod.rs`
- `plug-core/src/server/mod.rs`
- `plug-core/src/http/server.rs`
- `plug-core/src/http/session.rs`
- `plug-core/src/notifications.rs`
