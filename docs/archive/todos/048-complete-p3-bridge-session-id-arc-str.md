---
status: complete
priority: p3
issue_id: "048"
tags: [code-review, performance, consistency]
dependencies: []
---

# Bridge session_id fields should use Arc<str> not String

## Problem Statement

`HttpBridge` and `DaemonBridge` store `session_id: String` and clone it into every async block on each reverse request. The rest of the codebase uses `Arc<str>` for session/client IDs (e.g., `NotificationTarget`, `client_id` in `ProxyHandler`). Using `Arc<str>` would make clones O(1) reference count bumps instead of heap allocations.

Flagged by: performance-oracle.

## Findings

- `plug-core/src/http/server.rs`: `HttpBridge { session_id: String }`
- `plug/src/daemon.rs`: `DaemonBridge { session_id: String }`
- Both clone `session_id` in every `create_elicitation` / `create_message` call

## Proposed Solutions

Change `session_id` from `String` to `Arc<str>` in both bridges. Adjust construction sites.

- Effort: Trivial
- Risk: None

## Acceptance Criteria

- [ ] `HttpBridge.session_id` is `Arc<str>`
- [ ] `DaemonBridge.session_id` is `Arc<str>`
- [ ] All tests pass

## Work Log

| Date | Action | Learnings |
|------|--------|-----------|
| 2026-03-08 | Created from CE review | Consistency with existing `Arc<str>` pattern in codebase |
