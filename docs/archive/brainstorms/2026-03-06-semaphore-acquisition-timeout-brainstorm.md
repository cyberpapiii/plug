# Brainstorm: Semaphore Acquisition Timeout

**Date**: 2026-03-06
**Status**: Ready for planning

## What We're Building

A bounded wait for per-server semaphore acquisition in the tool-call path so callers do not hang indefinitely behind one slow in-flight request.

## Why This Approach

The current code waits forever on `acquire_owned().await` before the upstream call timeout starts. That means a single long-running request can stall every later request to the same server with no visible failure. The smallest fix is to add an explicit acquisition timeout and return a clear overload error.

## Key Decisions

- Keep the existing semaphore model; do not remove concurrency limiting
- Add a bounded semaphore acquisition timeout rather than a larger refactor
- Return an explicit server-overloaded style error instead of silent waiting
- Keep configuration surface minimal for now; prefer a small fixed timeout over a new config field in this pass

## Open Questions

- None blocking. The backlog item already narrows the scope sufficiently.

## Next

Write a focused implementation plan and execute it.
