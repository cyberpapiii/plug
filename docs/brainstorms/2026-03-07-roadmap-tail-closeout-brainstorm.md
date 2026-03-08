---
date: 2026-03-07
topic: roadmap-tail-closeout
---

# Roadmap Tail Closeout

## What We're Building

The remaining roadmap closeout tranche for `plug`: finish the only meaningful missing protocol surface (`resources/subscribe`), keep config reload explicitly honest instead of reopening a larger hot-reload redesign, and remove dead TUI dependencies that are still declared but unused.

This is not another broad feature phase. The transport, routing, daemon, HTTPS, meta-tool, and continuity work is already merged. What remains is a narrow finish pass that turns the last real protocol gap into working behavior and removes a small amount of stale package/document drift.

## Why This Approach

There are three residual items from the earlier strategic roadmap:

1. `resources/subscribe` was intentionally deferred and is still unimplemented.
2. Router/runtime reload still reports some settings as `restart required` instead of hot-applying them.
3. `ratatui`, `crossterm`, and `color-eyre` are still declared in the workspace even though they are no longer used.

Only the first of those is a meaningful product gap. Reopening reload hot-apply would widen scope into runtime draining and config architecture again. The smaller, better closeout is:
- implement resource subscriptions truthfully
- keep reload behavior explicitly bounded and documented
- remove dead dependency drift

## Key Decisions

- **Implement `resources/subscribe` and `resources/unsubscribe` together across all live downstream transports.**
  The protocol treats them as a pair. Shipping only subscribe would leave sticky per-client state with no release path.

- **Route resource subscriptions through the existing shared router and notification fan-out.**
  Reuse the `ProtocolNotification` bus and per-target delivery model already built for tools/progress/cancelled. Extend daemon IPC so `plug connect` participates instead of becoming the one product surface with reduced behavior.

- **Track subscriptions per downstream client/session, not per shared upstream session alone.**
  Upstream sessions are shared, but downstream resource update delivery must remain targeted to the subscribing stdio client or HTTP session.

- **Add daemon-backed notification parity rather than masking capabilities.**
  Product behavior should not split between direct stdio, HTTP, and daemon-backed `plug connect`. The daemon needs a push path for MCP notifications, not a capability downgrade.

- **Only subscribe upstream once per canonical resource URI.**
  Multiple downstream subscribers to the same routed resource should share one upstream subscription, with local reference tracking.

- **Keep reload semantics honest.**
  Do not reopen the broader “hot-apply router/runtime config” redesign in this tranche. Preserve `restart required` messaging and make that boundary explicit in docs if needed.

- **Remove dead TUI dependencies now.**
  If `ratatui`, `crossterm`, and `color-eyre` are unused, they should not stay in the workspace manifest or review context as if Phase 4 TUI is still active code.

## Open Questions

- Should `notifications/resources/list_changed` also be forwarded in this tranche, or should resource subscriptions stay limited to targeted `notifications/resources/updated`?
- Is there any remaining doc that still implies TUI code is present rather than merely planned historically?

## Next Steps

Write a focused implementation plan for:

1. `resources/subscribe` + `resources/unsubscribe` forwarding
2. targeted downstream `notifications/resources/updated`
3. daemon IPC notification push so `plug connect` has parity
4. truthful resource capability synthesis
5. dead TUI dependency removal
6. a small doc truth pass around reload and dependency state
