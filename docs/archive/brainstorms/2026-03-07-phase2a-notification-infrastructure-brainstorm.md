# Brainstorm: Phase 2A Notification Infrastructure

**Date**: 2026-03-07
**Status**: Ready for planning

## What We're Building

The next implementation tranche after `v0.1`: upstream notification handling plus downstream fan-out for the MCP messages real clients actually exercise first.

This phase is intentionally narrower than “full Phase 2.” It focuses on:

- upstream `tools/list_changed`
- downstream fan-out to stdio and HTTP clients
- the minimum correlation state needed for later cancellation and progress work

It does **not** include full resources/prompts forwarding, pagination, or broader spec-compliance work yet.

## Why This Approach

The strategic docs already established the right ordering:

- `v0.1` had to finish first
- the next highest-value gap is notification infrastructure
- cancellation and progress depend on the same request/session mapping layer

Current code facts support that sequencing:

- upstream clients are still `RunningService<RoleClient, ()>`, so all server-initiated notifications are dropped
- `EngineEvent` is observability-oriented, not protocol-notification-oriented
- stdio handler keeps only `client_type`
- HTTP has `session_id -> sse_sender` plumbing, but no notification delivery path
- `ToolRouter::call_tool()` has no request/session/progress correlation layer yet

That means the next clean move is to build the notification boundary first, not jump straight into resources/prompts or pagination.

## Key Decisions

- **Scope narrowly around notification infrastructure.**
  Do not broaden this into full resources/prompts forwarding yet.

- **Handle `tools/list_changed` first.**
  It is the highest-value server-initiated message because it directly affects tool visibility.

- **Add a first-class correlation layer.**
  Progress and cancellation cannot be implemented cleanly without preserving downstream request/session identity through `ToolRouter`.

- **Keep observability and protocol messaging separate.**
  `EngineEvent` can stay focused on internal state changes. Protocol notifications should use a dedicated payload path rather than overloading the current event bus.

- **Prefer additive extensions over structural rewrites.**
  Keep `UpstreamServer` as the upstream client holder. Extend handler/session layers around it.

## Resolved Questions

- **Should Phase 2A include all of Phase 2?** No
- **Should we start with resources/prompts?** No
- **Is HTTP SSE plumbing already enough?** No, registration exists but delivery/correlation does not
- **Should we reuse `EngineEvent` for protocol delivery?** Not as the only mechanism

## Open Questions

- What is the smallest shared request/session mapping that works for both stdio and HTTP?
- Should stdio fan-out hold a peer handle directly in `ProxyHandler`, or should notification emission be routed through a separate abstraction?
- Do we want one dedicated notification dispatcher inside `Engine`, or transport-specific subscribers fed by a shared internal message type?

## Next

Write a focused plan for:

1. upstream notification handler replacement
2. stdio + HTTP downstream delivery for `tools/list_changed`
3. minimal correlation state for later cancellation/progress

Everything else stays deferred until this infrastructure exists.
