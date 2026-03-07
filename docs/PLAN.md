# Current Plan

This document tracks the active product direction, not the original speculative phase map.

## Current Objective

Ship a truthful, stable `v0.1` of `plug`.

That means:

- the current daemon-backed CLI product is reliable
- security/truth bugs are fixed
- the docs match the code
- unsupported runtime changes are called out honestly

## Active v0.1 Work

Tracked in:

- `docs/plans/2026-03-06-strategic-assessment.md`
- `todos/029-complete-p1-serve-stdio-flag-honesty.md`
- `todos/030-complete-p1-make-prefix-behavior-explicit-for-v0-1.md`
- `todos/031-complete-p1-rewrite-core-docs-for-v0-1.md`
- `todos/032-complete-p1-final-v0-1-verification-and-release-boundary.md`

## Completed Stabilization Work

- recovered interrupted Phase 1 stability patch set
- sanitized tool descriptions in the tool cache
- made runtime and daemon behavior more truthful
- made reload semantics explicit for `v0.1`
- made serve/prefix behavior explicit in the command/config surface
- completed the `v0.1` verification and release boundary

## Deferred Until After v0.1

- bidirectional notification forwarding
- cancellation and progress propagation
- full resources/prompts forwarding
- pagination
- meta-tool discovery mode
- `rmcp` upgrade
- stateless/session abstraction work

## Next Phase After v0.1

After `v0.1` is verified and the docs gate is complete, the next implementation plan should focus on notification infrastructure first, then cancellation/progress, and only then the larger spec-surface work.
