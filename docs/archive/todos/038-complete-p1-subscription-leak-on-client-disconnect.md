---
status: complete
priority: p1
issue_id: "038"
tags: [code-review, architecture, resources, state-lifecycle]
dependencies: []
---

# Subscription leak on client disconnect

## Problem Statement

When a stdio or HTTP client disconnects without explicitly calling `resources/unsubscribe`, entries in `resource_subscriptions` DashMap remain orphaned. This prevents the last-subscriber upstream unsubscribe from ever firing and causes unbounded memory growth over time.

The plan explicitly called this out: "downstream disconnect/session removal must not leave orphaned local subscription entries or prevent upstream unsubscribe when the last subscriber goes away."

## Findings

- `plug-core/src/proxy/mod.rs`: The stdio notification fan-out task (line ~2067) breaks on peer send error but never calls `unsubscribe_resource()` for any subscriptions the disconnecting client held
- `plug-core/src/http/server.rs`: HTTP session cleanup (via `StatefulSessionStore` timeout) removes the session but does not clean up subscription entries
- The only subscription removal path is the explicit `unsubscribe_resource()` method called from `ServerHandler::unsubscribe`
- No `Drop` impl or cleanup hook on `ProxyHandler` that would clean subscriptions on disconnect

## Proposed Solutions

### Option A: Add cleanup method to ToolRouter, call from disconnect paths (Recommended)

Add `cleanup_target_subscriptions(&self, target: &NotificationTarget)` that iterates all entries and removes the target, calling upstream unsubscribe when transitioning to 0 subscribers.

Call this from:
- stdio fan-out break path (when peer send fails)
- HTTP session cleanup task

**Pros:** Minimal change, clear responsibility
**Cons:** O(n) over all URI keys on disconnect — acceptable for typical subscription counts
**Effort:** Small
**Risk:** Low

### Option B: Add reverse index (target → URIs) for O(1) cleanup

Maintain a second DashMap `target_subscriptions: DashMap<NotificationTarget, HashSet<String>>` alongside `resource_subscriptions` for bidirectional lookup.

**Pros:** O(1) cleanup on disconnect
**Cons:** More state to keep in sync, more complex
**Effort:** Medium
**Risk:** Medium (consistency between two maps)

## Technical Details

- **Affected files:** `plug-core/src/proxy/mod.rs`, `plug-core/src/http/server.rs`
- **Components:** ToolRouter subscription registry, stdio fan-out task, HTTP session store

## Acceptance Criteria

- [ ] Disconnecting stdio client triggers cleanup of all its subscriptions
- [ ] Expired HTTP session triggers cleanup of all its subscriptions
- [ ] Upstream unsubscribe fires when last subscriber disconnects ungracefully
- [ ] No orphaned entries remain in `resource_subscriptions` after client disconnect

## Work Log

- 2026-03-07: Identified during PR #30 review — no subscription cleanup on disconnect paths

## Resources

- PR #30: feat(resources): implement subscribe/unsubscribe forwarding
- Plan: docs/plans/2026-03-07-feat-roadmap-tail-closeout-plan.md (State lifecycle risks section)
