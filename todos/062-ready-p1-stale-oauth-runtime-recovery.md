# 062 - stale oauth runtime recovery

Status: complete
Priority: p1
Date: 2026-03-17

## Goal

Make single-server OAuth recovery use fresher persisted credentials without needing a full daemon restart.

## Why

Live Notion debugging showed a stale daemon/runtime state where persisted credentials still worked upstream, but the running daemon stayed failed until restart.

## Tasks

- [x] teach OAuth lookup to prefer fresher persisted credentials over stale cached credentials
- [x] add regression coverage for stale-cache/newer-persisted-token recovery
- [x] verify `plug-core` auth/runtime tests stay green
- [x] record outcome and reasoning

## Verification

- `cargo test -p plug-core oauth -- --nocapture`
- `cargo test -p plug-core -- --nocapture`

## Notes

This is a targeted runtime/auth recovery fix, not a broader auth architecture rewrite.

Outcome:
- `current_or_stored_access_token(...)` now refreshes cache from a newer mirrored
  token file instead of blindly trusting stale in-memory credentials.
- Added a regression test covering the exact stale-cache/newer-persisted-token
  scenario that matched the live Notion failure mode.
