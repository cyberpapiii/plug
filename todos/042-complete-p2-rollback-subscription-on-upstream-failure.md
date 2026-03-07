---
status: complete
priority: p2
issue_id: "042"
tags: [code-review, architecture, resources, error-handling]
dependencies: []
---

# Rollback local subscription on upstream subscribe failure

## Problem Statement

In `subscribe_resource()`, the local subscriber is inserted into the DashMap *before* the upstream subscribe call. If the upstream call fails, the local entry remains with a subscriber that believes it is subscribed, but no upstream subscription exists. Subsequent subscribers see `is_first == false` and skip the upstream call, so no client ever receives notifications for that URI.

## Findings

- `plug-core/src/proxy/mod.rs:494-511`: `entry.insert(target)` at line 499, `drop(entry)` at 500, upstream call at 502-511
- If upstream `.subscribe().await` returns `Err`, the target remains in the registry
- Flagged by: security-sentinel, agent-native-reviewer

## Proposed Solutions

### Option A: Rollback on failure (Recommended)

```rust
if is_first {
    if let Err(e) = upstream.client.peer().subscribe(...).await {
        if let Some(mut entry) = self.resource_subscriptions.get_mut(uri) {
            entry.remove(&target);
            if entry.is_empty() {
                drop(entry);
                self.resource_subscriptions.remove(uri);
            }
        }
        return Err(e);
    }
}
```

**Effort:** Small
**Risk:** Low

## Acceptance Criteria

- [ ] Failed upstream subscribe removes the local subscriber entry
- [ ] Empty entries are cleaned up after rollback

## Work Log

- 2026-03-07: Identified during PR #30 review by security-sentinel and agent-native-reviewer
