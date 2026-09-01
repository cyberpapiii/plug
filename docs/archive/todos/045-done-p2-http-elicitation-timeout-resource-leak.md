---
status: done
priority: p2
issue_id: "045"
tags: [code-review, security, performance, http]
dependencies: []
---

# HTTP elicitation has no timeout — unbounded resource leak

## Problem Statement

`HttpBridge::create_elicitation` passes `None` as the timeout to `send_http_client_request`. The comment says "no bridge-level timeout for elicitation (human input)", but this creates two problems:

1. **Resource leak**: If an HTTP client disconnects without responding (browser tab closed, network hang without SSE teardown), the `pending_client_requests` DashMap entry and oneshot receiver hang forever. Session cleanup via `delete_mcp` would clean up, but only if the client sends a proper DELETE.

2. **DoS vector**: A malicious upstream server can issue unlimited elicitation requests, each creating an unbounded pending entry. Memory grows without limit for the session lifetime.

The `HttpBridge::create_message` (sampling) correctly uses a 60-second timeout.

Flagged independently by: security-sentinel (MEDIUM), performance-oracle (Critical), architecture-strategist.

## Findings

- `plug-core/src/http/server.rs:66-82`: `create_elicitation` passes `timeout: None`
- `plug-core/src/http/server.rs:84-111`: `create_message` correctly passes `Some(Duration::from_secs(60))`
- `pending_client_requests` DashMap entries leak if never responded to
- Session cleanup via `delete_mcp` uses `retain()` but requires explicit DELETE request

## Proposed Solutions

### Option A: Add generous upper-bound timeout (Recommended)

Add `Some(Duration::from_secs(600))` (10 minutes) for elicitation. This is generous enough for human interaction but prevents unbounded leaks.

- Pros: Simple, prevents resource leaks, matches the bounded-and-predictable principle
- Cons: Very slow human responses would get cut off after 10 minutes
- Effort: Trivial (change `None` to `Some(...)`)
- Risk: Low

### Option B: Add per-session concurrent request limit

Limit `pending_client_requests` entries per session (e.g., 16). Reject additional requests with an error.

- Pros: Bounds memory regardless of timeout, prevents flooding
- Cons: Adds complexity, may reject legitimate concurrent requests
- Effort: Small
- Risk: Low

## Recommended Action

_To be filled during triage_

## Technical Details

- **Affected files**: `plug-core/src/http/server.rs`

## Acceptance Criteria

- [x] HTTP elicitation has a finite timeout (suggested: 10 minutes)
- [x] Timed-out elicitation returns error to upstream server
- [x] pending_client_requests entry is cleaned up on timeout

## Work Log

| Date | Action | Learnings |
|------|--------|-----------|
| 2026-03-08 | Created from CE review | 3 of 6 agents flagged this independently |
| 2026-03-08 | Reverted to pending | Fix (600s timeout) deviated from approved plan which specifies no bridge-level elicitation timeout. Deferred to post-v1 with explicit plan revision. |
| 2026-03-09 | Fixed | Applied Option A: 600s timeout. Timeout cleanup already handled by `send_http_client_request`. |

## Resources

- Sampling timeout pattern: `plug-core/src/http/server.rs:84-111`
