---
status: pending
priority: p2
issue_id: "039"
tags: [code-review, architecture, resources, state-consistency]
dependencies: []
---

# Stale subscriptions after route refresh

## Problem Statement

When upstream server routes are refreshed (reconnect, config reload), the `resource_subscriptions` DashMap may contain entries keyed by URIs that no longer exist in the new route cache. These orphaned entries will never match incoming notifications and will never be cleaned up.

## Findings

- `plug-core/src/proxy/mod.rs`: `refresh_tools_for_server()` updates the ArcSwap route cache but does not inspect or clean `resource_subscriptions`
- `subscribe_resource()` resolves routes from the cache snapshot — stale subscriptions from a previous cache won't match new routes
- If an upstream server disconnects and reconnects with different resource URIs, old subscription entries persist

## Proposed Solutions

### Option A: Clear subscriptions for a server on refresh (Recommended)

When `refresh_tools_for_server()` runs, identify subscriptions that were routed to that server and remove them if the URI is no longer in the new route cache.

**Pros:** Prevents unbounded stale state
**Cons:** Clients lose subscriptions silently on server refresh
**Effort:** Small
**Risk:** Low

### Option B: Re-subscribe on refresh

After refresh, re-subscribe upstream for any subscriptions that still have valid routes.

**Pros:** Subscription continuity across reconnects
**Cons:** More complex, may not be needed for v0.1
**Effort:** Medium
**Risk:** Medium

## Acceptance Criteria

- [ ] Server reconnect/refresh cleans stale subscription entries
- [ ] No unbounded growth of subscription entries across reconnections

## Work Log

- 2026-03-07: Identified during PR #30 review

## Resources

- PR #30
