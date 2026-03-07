# Brainstorm: Health Checks vs Circuit Breaker

**Date**: 2026-03-06
**Status**: Resolved during verification

## What We're Building

No new feature. This brainstorm exists to re-evaluate whether the old “health checks and circuit breaker amplify each other” finding is still true in the current branch state.

## Why This Approach

The backlog item predates two important changes:

- tool-call timeouts no longer trip the circuit breaker
- health probes use `list_all_tools()` directly and do not consult the circuit breaker

That means the original “slow server gets double-penalized” path is no longer the current behavior.

## Key Decisions

- Treat todo `027` as stale/overstated in its current wording
- Do not force a single-failure-tracking refactor into this branch
- If dual-state tracking becomes a real problem later, create a fresh todo with current evidence

## Resolved Questions

- **Do health probes go through the circuit breaker?** No
- **Do tool-call timeouts still trip the circuit breaker?** No
- **Should this branch remove `HealthState` entirely?** No

## Next

Close the stale todo and continue to genuinely unresolved work.
