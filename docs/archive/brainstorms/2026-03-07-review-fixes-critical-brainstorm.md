# Brainstorm: Critical Review Fixes After Phase 3

**Date**: 2026-03-07
**Status**: Ready for planning

## What We're Building

A narrow follow-up tranche to address the verified critical review findings from the completed
Phase 2 / Phase 3 program.

This tranche focuses on:

- fixing the authenticated HTTP upstream regression
- restoring daemon IPC parity for resources/prompts
- eliminating the active-call cleanup leak risk
- fixing low-risk truth gaps discovered in the same review pass

This tranche does **not** include:

- blanket rate limiting of tool calls
- tool-quarantine / approval UX
- larger optimization-only cleanups

## Why This Approach

The review surfaced a mix of real bugs, stale findings, and future ideas.

The right move is to take only the issues that are both:

1. still true on current `main`
2. severe enough to justify immediate action

That keeps the branch surgical and avoids turning a review-fix pass into another open-ended feature
wave.

## Key Decisions

- **Do not add blanket tool-call rate limiting.**
  Agent-heavy workflows depend on high tool-call throughput.

- **Fix daemon IPC parity, not just capability advertising.**
  The proxy path should actually serve resources/prompts, not merely claim them.

- **Treat active-call cleanup as a correctness issue, not an optimization.**

- **Defer `resources/subscribe` and route-lookup optimization.**
  They are real follow-ups, but not in the same severity class.

## Resolved Questions

- **Should tool calls be rate limited?** No
- **Should this branch fix only stale docs?** No
- **Should daemon IPC parity be addressed at the dispatch path?** Yes

## Open Questions

None. The slice is narrow enough to proceed directly.
