# Stale OAuth Runtime Recovery Plan

Date: 2026-03-17

Purpose: make a single OAuth-backed upstream recover from fresher persisted credentials without requiring a full daemon restart.

## Problem

We reproduced a live case where `notion` showed `Failed` overnight even though:

- persisted OAuth credentials existed
- the stored access token still worked against the upstream MCP server
- a full `plug stop && plug start` immediately restored health

That points to stale daemon-side runtime/auth state rather than broken credentials or upstream outage.

## Hypothesis

The daemon can retain an older in-memory OAuth credential cache even after fresher credentials have been persisted by another process or later auth flow.

Today `current_or_stored_access_token(server_name)` returns the in-memory token immediately on cache hit. That is good for avoiding repeated secure-store reads, but it means a single-server reconnect can keep reusing a stale token until the whole daemon process restarts and clears cache.

## Goals

1. Prefer fresher persisted OAuth credentials over stale in-memory cache during reconnect/startup.
2. Preserve the existing non-interactive preference for the mirrored 0600 token file over keychain prompts.
3. Add regression coverage for the stale-cache/newer-persisted-credentials case.
4. Document the reasoning so future auth/runtime work doesn’t regress this seam.

## Proposed Fix

1. Teach the per-server credential store how to compare cached credentials against persisted credentials loaded directly from backing stores.
2. Update `current_or_stored_access_token(server_name)` so it refreshes cache from persisted credentials when the persisted record is newer than the cached record.
3. Keep the file-first read order so short-lived CLI and reconnect paths stay non-interactive on macOS.
4. Add tests that simulate:
   - stale cached token in the global store
   - newer token persisted by another store/process
   - successful rehydration to the newer token on reconnect lookup

## Expected Outcome

After this change:
- explicit server restart/reconnect can recover from fresh persisted OAuth credentials
- a full daemon restart is no longer required just to clear stale token cache
- live auth/runtime surfaces should be less likely to get stuck in misleading failure states

## Likely Files

- `plug-core/src/oauth.rs`
- `docs/plans/2026-03-17-stale-oauth-runtime-recovery-plan.md`
- `todos/062-ready-p1-stale-oauth-runtime-recovery.md`

## Verification

- `cargo test -p plug-core oauth -- --nocapture`
- `cargo test -p plug-core -- --nocapture`
- focused live smoke where practical:
  - `plug auth status`
  - `plug status`

## Result

Implemented on `main` by teaching OAuth token lookup to compare cached
credentials against the mirrored token file and refresh cache when the persisted
record is newer. This keeps reconnect/startup paths non-interactive while
closing the stale-daemon-cache recovery seam we reproduced with Notion.
