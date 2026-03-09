---
status: pending
priority: p2
issue_id: "051"
tags: [code-review, oauth, security, correctness]
dependencies: []
---

# Replace string-based auth failure classification with typed path

## Problem Statement

Auth failure classification in `refresh_access_token()` (oauth.rs) and the reconnect error handler
in `run_refresh_loop` (engine.rs) both use string matching on error messages to decide whether a
failure is an auth error vs. a transient error. The two classifiers use different string sets and can
diverge. Specific risks:

- `"authorization"` matches "failed to fetch authorization server metadata" (metadata fetch failure
  misclassified as auth rejection)
- `"401"` can false-positive on URLs containing port 4018 or similar
- The two sites (oauth.rs and engine.rs) are not guaranteed to stay in sync

This matters because misclassifying a transient error as an auth error triggers `mark_auth_required`,
which disables the server until manual intervention.

## Findings

- Identified by security-sentinel and code-simplicity-reviewer during PR #42 CE review
- oauth.rs line ~659: matches `invalid_grant`, `invalid_token`, `unauthorized`, `authorization`
- engine.rs reconnect error handler: matches `401`, `unauthorized`, `forbidden`
- No shared function or enum for this classification

## Proposed Solutions

### Option A: Shared classifier function (Recommended)

Extract a single `fn is_auth_error(err: &str) -> bool` used by both sites. Tighten the patterns
(e.g. `"authorization_error"` instead of `"authorization"`, word-boundary matching for `"401"`).

**Pros:** Single source of truth, easy to test, minimal refactor
**Cons:** Still string-based under the hood
**Effort:** Small
**Risk:** Low

### Option B: Typed error enum from rmcp

If rmcp exposes structured OAuth error types, match on those instead of stringified errors. Would
eliminate the classification problem entirely.

**Pros:** Correct by construction
**Cons:** Depends on rmcp's error surface; may require upstream contribution
**Effort:** Medium
**Risk:** Low

## Acceptance Criteria

- [ ] Single classifier function used by both oauth.rs and engine.rs
- [ ] `"authorization"` no longer matches metadata fetch failures
- [ ] `"401"` no longer false-positives on URLs/ports
- [ ] Unit tests for classification edge cases

## Work Log

- 2026-03-09: Identified during PR #42 CE review (security-sentinel, code-simplicity-reviewer)

## Resources

- PR #42
- `plug-core/src/oauth.rs` — `refresh_access_token()` error classification
- `plug-core/src/engine.rs` — `run_refresh_loop` reconnect error classification
