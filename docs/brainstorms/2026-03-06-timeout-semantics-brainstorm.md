# Brainstorm: Timeout Semantics

**Date**: 2026-03-06
**Status**: Resolved during verification

## What We're Building

Nothing new. This brainstorm exists to verify whether the old timeout issue still represents real work.

## Why This Approach

The tracked todo claimed startup and tool calls shared one timeout. Current code review shows that the split already exists:

- `timeout_secs` for upstream startup in `plug-core/src/server/mod.rs`
- `call_timeout_secs` for tool calls in `plug-core/src/proxy/mod.rs`

## Key Decisions

- Treat todo `022` as stale backlog, not active implementation work
- Verify the current split with code inspection and tests
- Close the todo instead of re-planning already-shipped behavior

## Resolved Questions

- **Is there still a single timeout field?** No
- **Do startup and tool calls use different config values?** Yes
- **Should we create a new implementation plan?** No

## Next

Close the stale todo and continue to the next unresolved backlog item.
