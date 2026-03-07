---
title: "feat: roadmap tail closeout"
type: feat
status: active
date: 2026-03-07
origin: docs/brainstorms/2026-03-07-roadmap-tail-closeout-brainstorm.md
---

# Roadmap Tail Closeout

## Overview

Finish the remaining high-signal tail of the original roadmap: implement truthful resource subscriptions, keep reload boundaries explicit, and remove dead TUI dependencies that no longer correspond to live code.

## Problem Statement / Motivation

The major Phase 1-3 roadmap work is already merged, but three residual items remain:

- `resources/subscribe` is still missing even though resource forwarding exists
- reload still intentionally reports some config changes as `restart required`
- `ratatui`, `crossterm`, and `color-eyre` are still declared in the workspace with no code using them

Only the first is a meaningful protocol gap. The closeout goal is to finish that gap without reopening a broader runtime redesign, and to leave the repo in a truthful state afterward.

## Proposed Solution

### Scope

This plan includes:

- `resources/subscribe` forwarding
- `resources/unsubscribe` forwarding
- targeted downstream `notifications/resources/updated`
- truthful `resources.subscribe` capability synthesis
- honest daemon IPC capability masking for resource subscriptions
- focused documentation cleanup for reload/dependency truth
- removal of dead TUI dependencies from the workspace and stale review context

This plan explicitly excludes:

- hot-applying router/runtime config during reload
- draining redesign for reload during active calls
- speculative TUI resurrection or redesign
- generic resource event fan-out beyond `notifications/resources/updated`

### Technical Approach

1. **Add resource subscription bookkeeping to the shared router**
   Extend `ToolRouter` with a subscription registry keyed by canonical upstream resource URI.

   The registry needs to track:
   - routed resource name/URI → upstream server + canonical URI
   - upstream canonical URI → downstream subscriber set
   - downstream subscriber target → subscribed routed resources

   Subscriber targets should reuse the existing targeting model:
   - stdio client: `NotificationTarget::Stdio { client_id }`
   - HTTP client: `NotificationTarget::Http { session_id }`

2. **Subscribe upstream only on first local subscriber**
   When a downstream client subscribes to a routed resource:
   - resolve the route
   - verify the backing upstream advertises `resources.subscribe = true`
   - add the local subscriber
   - call upstream `subscribe` only when transitioning from 0 → 1 local subscribers

   Likewise, `unsubscribe` should only call upstream when transitioning from 1 → 0.

3. **Fan out upstream `notifications/resources/updated` to only subscribed downstream targets**
   Extend the existing upstream `ClientHandler` path and internal `ProtocolNotification` enum to carry:
   - `ResourceUpdated { target, params }`
   - optionally `ResourceListChanged` only if the implementation ends up trivial and truthful

   Use the same stdio/HTTP notification fan-out infrastructure already used for:
   - `tools/list_changed`
   - `notifications/progress`
   - `notifications/cancelled`

4. **Add downstream handler methods**
   Implement `ServerHandler::subscribe` and `ServerHandler::unsubscribe` in:
   - `plug-core/src/proxy/mod.rs` for direct stdio/HTTP server use

   Do **not** force this through daemon IPC in this tranche. The daemon-backed
   path is still request/response only and has no push channel for targeted
   MCP notifications. Instead, make that capability boundary explicit so daemon-
   backed clients do not advertise a subscription feature they cannot receive.

5. **Make capability synthesis truthful**
   Change merged `ResourcesCapability` synthesis so:
   - `subscribe: Some(true)` only when at least one healthy upstream actually supports resource subscriptions and the downstream subscribe path is implemented
   - otherwise `subscribe: None`

   Do not advertise list-changed support unless it is truly forwarded.

   For daemon-backed IPC clients, mask `resources.subscribe` back to `None`
   until IPC supports targeted server-to-client notifications.

6. **Remove dead TUI dependencies and stale context**
   Remove unused workspace dependencies:
   - `ratatui`
   - `crossterm`
   - `color-eyre`

   Update any lightweight docs or local review-context files that still imply the TUI is active code, if they are now misleading.

7. **Keep reload semantics explicit, not magical**
   Do not change runtime reload behavior. If any touched doc still suggests router/runtime config hot-applies, tighten it to the already-implemented `restart required` boundary.

## System-Wide Impact

- **Interaction graph**
  upstream resource notification → upstream client handler → internal `ProtocolNotification` → stdio/HTTP targeted fan-out → only subscribed downstream client receives the update

- **Error propagation**
  subscribing to a routed resource backed by a non-subscribing upstream should return an MCP error, not a silent success

- **State lifecycle risks**
  downstream disconnect/session removal must not leave orphaned local subscription entries or prevent upstream unsubscribe when the last subscriber goes away

- **API surface parity**
  resource subscriptions must work through direct stdio and HTTP. Daemon IPC
  clients must remain honest about not supporting subscription delivery yet.

- **Integration test scenarios**
  - two downstream subscribers to the same resource share one upstream subscribe
  - unsubscribing one subscriber does not remove updates for the other
  - removing the final subscriber triggers upstream unsubscribe
  - HTTP session receives `notifications/resources/updated` only for subscribed resource(s)
  - stdio client receives `notifications/resources/updated` only for subscribed resource(s)
  - daemon IPC proxy supports subscribe/unsubscribe parity

## Acceptance Criteria

- [x] `resources/subscribe` works end-to-end for routed upstream resources
- [x] `resources/unsubscribe` works end-to-end and cleans up upstream subscriptions when appropriate
- [x] upstream `notifications/resources/updated` are routed only to subscribed downstream targets
- [x] direct stdio and HTTP both support resource subscription parity
- [x] daemon IPC-backed clients do not advertise `resources.subscribe` until IPC notification push exists
- [x] merged capabilities advertise `resources.subscribe` truthfully
- [x] dead TUI dependencies are removed from the workspace manifest (PR #29)
- [ ] no touched docs imply router/runtime reload behavior that the code does not implement
- [x] focused tests cover subscribe/unsubscribe lifecycle and targeted notification delivery

## Dependencies & Risks

- Shared upstream sessions make per-downstream subscription accounting the main correctness risk
- Downstream session cleanup must not leak subscriptions
- Resource URIs must remain canonical; local routing identity cannot mutate the upstream URI in notifications
- This tranche should not sprawl into full reload/draining redesign

## Sources & References

- **Origin brainstorm:** [docs/brainstorms/2026-03-07-roadmap-tail-closeout-brainstorm.md](docs/brainstorms/2026-03-07-roadmap-tail-closeout-brainstorm.md)
- [docs/brainstorms/2026-03-07-phase2c-resources-prompts-pagination-brainstorm.md](docs/brainstorms/2026-03-07-phase2c-resources-prompts-pagination-brainstorm.md)
- [docs/plans/2026-03-07-feat-phase2c-resources-prompts-pagination-plan.md](docs/plans/2026-03-07-feat-phase2c-resources-prompts-pagination-plan.md)
- [docs/solutions/integration-issues/review-fixes-critical-http-auth-ipc-parity-20260307.md](docs/solutions/integration-issues/review-fixes-critical-http-auth-ipc-parity-20260307.md)
- `plug-core/src/proxy/mod.rs`
- `plug-core/src/http/server.rs`
- `plug/src/daemon.rs`
- `plug/src/ipc_proxy.rs`
- `plug-core/src/notifications.rs`
