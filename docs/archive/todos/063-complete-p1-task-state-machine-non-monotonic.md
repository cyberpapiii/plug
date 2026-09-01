---
status: complete
priority: p1
issue_id: "063"
tags: [code-review, tasks, correctness, concurrency, mcp]
dependencies: []
---

# Make task state transitions monotonic

## Problem Statement

The new task lifecycle store allows terminal task states to be overwritten after completion. A late cancel, failure, or upstream sync can change a completed task into `Cancelled` or `Failed` and clear its result.

This breaks basic task semantics and can make a successfully completed task appear to have failed or been cancelled after the fact.

## Findings

- `TaskStore::complete`, `TaskStore::fail`, and `TaskStore::mark_cancelled` all mutate task state unconditionally in [tasks.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/tasks.rs).
- `sync_from_upstream_for_owner` also overwrites local task status and metadata without guarding terminal transitions in [tasks.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/tasks.rs).
- Review synthesis: this can cause result loss and inconsistent lifecycle reporting if a cancel arrives after completion or if an upstream update races local completion.

## Proposed Solutions

### Option 1: Enforce a strict terminal-state guard in `TaskStore`

**Approach:** Once a task reaches `Completed`, `Failed`, or `Cancelled`, reject further transitions unless they preserve the same terminal state.

**Pros:**
- Fixes the correctness issue at the source
- Keeps all transports consistent
- Small implementation surface

**Cons:**
- Requires clear rules for passthrough sync edge cases

**Effort:** 2-4 hours

**Risk:** Low

---

### Option 2: Add transition-specific compare-and-set rules

**Approach:** Encode an explicit state machine allowing only valid forward transitions such as `Working -> Completed`, `Working -> Failed`, `Working -> Cancelled`.

**Pros:**
- Strongest long-term correctness model
- Easier to reason about future task states like `InputRequired`

**Cons:**
- More code and more test cases

**Effort:** 4-6 hours

**Risk:** Medium

## Recommended Action

To be filled during triage.

## Technical Details

**Affected files:**
- [tasks.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/tasks.rs)

**Related components:**
- Task wrapper mode
- Upstream passthrough mode
- HTTP / stdio / IPC task retrieval and cancellation

## Resources

- **Branch:** `feat/core-mcp-tasks-support`
- **Commits:** `aa09961`, `6571950`, `476dac9`

## Acceptance Criteria

- [ ] A completed task cannot later become cancelled or failed
- [ ] Cancelling an already completed task returns a stable terminal response without clearing payload
- [ ] Upstream sync cannot regress a terminal local task into a different terminal state
- [ ] Tests cover completion/cancel/failure races for both wrapper and passthrough tasks

## Work Log

### 2026-03-22 - Code review capture

**By:** Codex / ce:review

**Actions:**
- Reviewed task lifecycle mutations in [tasks.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/tasks.rs)
- Cross-checked review-agent findings against branch changes
- Captured the issue as a merge-blocking correctness finding

**Learnings:**
- The current task store behaves more like last-write-wins state than a lifecycle state machine
- Terminal task states need explicit invariants, not just best-effort updates

### 2026-03-22 - Fix implemented

**By:** Codex / ce:work

**Actions:**
- Added terminal-state guards in [tasks.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/tasks.rs)
- Prevented late `complete`, `fail`, `cancel`, and upstream sync paths from regressing terminal tasks
- Added a monotonic lifecycle regression test

**Learnings:**
- The right default for task lifecycle mutations is fail-safe no-op once terminal
- Upstream sync needs the same monotonicity rule as local task transitions
