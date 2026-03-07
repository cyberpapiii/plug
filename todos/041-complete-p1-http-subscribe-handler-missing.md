---
status: complete
priority: p1
issue_id: "041"
tags: [code-review, architecture, resources, http, parity]
dependencies: []
---

# HTTP subscribe/unsubscribe handler missing

## Problem Statement

The HTTP server's `handle_request` function has no `ClientRequest::SubscribeRequest` or `ClientRequest::UnsubscribeRequest` match arms. HTTP clients attempting to subscribe to resource updates receive a `METHOD_NOT_FOUND` JSON-RPC error, despite the server advertising `resources.subscribe: true` in capabilities.

This breaks the PR's acceptance criterion: "direct stdio and HTTP both support resource subscription parity."

## Findings

- `plug-core/src/http/server.rs:478`: catch-all `_ =>` arm returns `METHOD_NOT_FOUND` for any unhandled request type
- No `SubscribeRequest` or `UnsubscribeRequest` arms exist in the HTTP handler
- The stdio path correctly implements subscribe/unsubscribe via `ProxyHandler`'s `ServerHandler` impl
- HTTP capabilities synthesis still advertises `resources.subscribe: Some(true)` when upstream supports it
- HTTP notification fan-out for `ResourceUpdated` was correctly added (line 98-112) but can never fire since no HTTP client can subscribe

## Proposed Solutions

### Option A: Add SubscribeRequest/UnsubscribeRequest arms to HTTP handle_request (Recommended)

Follow the pattern of `ReadResourceRequest` — extract session_id, validate session, call `router.subscribe_resource()` / `router.unsubscribe_resource()` with `NotificationTarget::Http { session_id }`.

**Pros:** Straightforward, follows existing patterns
**Cons:** None
**Effort:** Small
**Risk:** Low

## Technical Details

- **Affected file:** `plug-core/src/http/server.rs`
- **Pattern to follow:** `ClientRequest::ReadResourceRequest` arm at line 432

## Acceptance Criteria

- [ ] HTTP clients can call `resources/subscribe` and receive success
- [ ] HTTP clients can call `resources/unsubscribe` and receive success
- [ ] HTTP clients receive `notifications/resources/updated` via SSE for subscribed resources
- [ ] Test confirms HTTP subscription parity with stdio

## Work Log

- 2026-03-07: Identified by security-sentinel agent during PR #30 review

## Resources

- PR #30
