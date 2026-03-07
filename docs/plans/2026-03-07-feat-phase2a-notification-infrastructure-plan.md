---
title: "feat: phase 2a notification infrastructure"
type: feat
status: active
date: 2026-03-07
origin: docs/brainstorms/2026-03-07-phase2a-notification-infrastructure-brainstorm.md
---

# Phase 2A Notification Infrastructure

## Overview

Build the minimum protocol-notification infrastructure needed after `v0.1`: receive upstream MCP notifications, refresh tool state on `tools/list_changed`, and fan the notification out to connected stdio and HTTP clients.

## Problem Statement / Motivation

`plug` currently has a strong tool-call path but no real server-initiated notification path:

- upstream notifications are dropped by the `()` client handler
- stdio handler discards request context after initialize
- HTTP registers SSE senders but never delivers onto them
- there is no correlation layer for later progress/cancellation work

Without this, tool-list changes upstream never reach downstream clients and the next spec-compliance tranche has no clean place to attach cancellation/progress routing.

## Proposed Solution

### Scope

This phase includes:

- replace the upstream `()` client handler with a real notification-aware handler
- handle `tools/list_changed` end to end
- store enough downstream session/request identity to support later cancellation/progress work

This phase excludes:

- full resources/prompts forwarding
- pagination
- meta-tool mode
- full capability synthesis

### Technical Approach

1. **Upstream notification handler**
   Replace `RunningService<RoleClient, ()>` with a handler that receives server-initiated notifications and forwards them into `plug`’s internal notification path.

2. **Dedicated protocol-notification payload path**
   Keep `EngineEvent` for observability. Add a separate internal notification type/channel for protocol messages so transport fan-out is not built on top of the observability enum.

3. **HTTP delivery**
   Extend `SessionManager` so each session can receive serialized JSON-RPC notifications through its SSE sender.

4. **stdio delivery**
   Extend `ProxyHandler` so it can retain what it needs from `RequestContext<RoleServer>` to emit downstream notifications to the connected stdio client.

5. **Correlation state**
   Add a first-class correlation layer that preserves downstream request/session identity through `ToolRouter` so later progress/cancellation work has a foundation.

## System-Wide Impact

- **Interaction graph**
  Upstream notification handler -> internal protocol notification dispatcher -> stdio/HTTP transport fan-out.

- **Error propagation**
  Delivery failures should be logged per transport/session but should not crash the engine or poison unrelated clients.

- **State lifecycle risks**
  Correlation entries must be cleaned up on request completion and session teardown to avoid stale progress/cancellation routing.

- **API surface parity**
  Both stdio and HTTP must receive the same `tools/list_changed` semantics even though the transport mechanics differ.

- **Integration test scenarios**
  - upstream `tools/list_changed` triggers cache refresh and reaches stdio client
  - upstream `tools/list_changed` reaches HTTP SSE client
  - disconnected HTTP session with no SSE sender does not crash fan-out
  - correlation entries are removed after request completion

## Acceptance Criteria

- [x] Upstream notifications are no longer silently dropped
- [x] `tools/list_changed` triggers tool cache refresh
- [x] Connected stdio clients receive downstream `tools/list_changed`
- [x] Connected HTTP SSE clients receive downstream `tools/list_changed`
- [x] A minimal request/session correlation layer exists for later cancellation/progress work
- [x] Focused tests cover both stdio and HTTP fan-out

## Dependencies & Risks

- Highest-risk step is replacing the upstream `()` handler cleanly within current `rmcp` APIs
- The correlation layer should stay minimal; do not broaden it into full cancellation/progress behavior yet
- Transport fan-out must not regress the current `v0.1` tool-call path

## Sources & References

- **Origin brainstorm:** `docs/brainstorms/2026-03-07-phase2a-notification-infrastructure-brainstorm.md`
- `docs/plans/2026-03-06-feat-strategic-stabilize-comply-compete-plan.md`
- `docs/plans/2026-03-06-strategic-assessment.md`
- `plug-core/src/server/mod.rs`
- `plug-core/src/engine.rs`
- `plug-core/src/proxy/mod.rs`
- `plug-core/src/http/session.rs`
- `plug-core/src/http/server.rs`
