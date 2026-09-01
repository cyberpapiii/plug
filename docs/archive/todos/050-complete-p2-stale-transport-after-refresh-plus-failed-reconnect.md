---
status: complete
priority: p2
issue_id: "050"
tags: [code-review, oauth, reliability, engine]
dependencies: []
---

# Stale transport after successful refresh + failed reconnect

## Problem Statement

In `run_refresh_loop` (engine.rs), if `refresh_access_token()` succeeds but the subsequent
`reconnect_server()` fails with a non-auth error, the loop falls back to the full refresh-window
sleep (potentially hours). During that window the server keeps running with the old expired
transport while the fresh token sits unused in the credential store. The next loop iteration will
attempt another OAuth refresh (which may fail with `invalid_grant` since the previous refresh
already consumed the refresh token).

This matters because a transient network blip during reconnect could leave a server silently
degraded for the entire refresh interval.

## Findings

- Identified by architecture-strategist agent during PR #42 CE review
- The `Refreshed` arm in the match calls `reconnect_server()` but does not distinguish reconnect
  failure from refresh failure — both fall through to the default sleep
- A flag or state to "skip refresh, retry reconnect only" on the next iteration would fix this

## Proposed Solutions

### Option A: Reconnect-retry flag (Recommended)

Add a `reconnect_pending: bool` flag to the refresh loop. When refresh succeeds but reconnect
fails, set the flag and use a short retry interval (e.g. 30s) instead of the full refresh window.
On retry, skip the refresh step and go straight to reconnect.

**Pros:** Simple, targeted fix; token is not re-refreshed unnecessarily
**Cons:** Adds loop state
**Effort:** Small
**Risk:** Low

### Option B: Immediate reconnect retry with backoff

On reconnect failure after successful refresh, retry reconnect 2-3 times with exponential backoff
before falling back to the full sleep.

**Pros:** Handles transient network issues inline
**Cons:** Delays the loop iteration; may block other servers' refresh checks
**Effort:** Small
**Risk:** Low

## Acceptance Criteria

- [x] Successful refresh + failed reconnect retries reconnect without re-refreshing the token
- [x] Retry uses a shorter interval than the full refresh window
- [ ] If reconnect continues to fail, eventually transitions to AuthRequired (auth failures do transition; non-auth transient failures loop indefinitely — pre-existing behavior, tracked separately)

## Work Log

- 2026-03-09: Identified during PR #42 CE review (architecture-strategist agent)
- 2026-03-09: Fixed via PR #44 using Option A (reconnect_pending flag). CE review passed, CI 7/7 green.

## Resources

- PR #42
- `plug-core/src/engine.rs` — `run_refresh_loop` match arm for `RefreshResult::Refreshed`
