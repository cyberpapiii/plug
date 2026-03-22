---
status: pending
priority: p2
issue_id: "066"
tags: [code-review, tasks, performance, memory, daemon]
dependencies: []
---

# Harden task-store lifecycle and scaling behavior

## Problem Statement

The task store currently keeps completed payloads for up to one hour, never expires in-flight tasks, and prunes only on task-store access. All task operations serialize through a single mutex and `tasks/list` scans, clones, and sorts the full task map before pagination.

This is acceptable at small scale but creates avoidable latency and memory pressure under polling or high task volume.

## Findings

- Completed payloads are retained in-memory with a one-hour TTL in [tasks.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/tasks.rs).
- `prune_expired()` never evicts `Working` or `InputRequired` tasks, so abandoned in-flight tasks can survive indefinitely.
- `list_for_owner()` scans and sorts the whole map before applying pagination in [tasks.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/tasks.rs).
- Review also flagged the daemon’s accept path as potentially unbounded for unregistered sockets in [daemon.rs](/Users/robdezendorf/Documents/GitHub/plug/plug/src/daemon.rs), which compounds resource pressure if task usage increases.

## Proposed Solutions

### Option 1: Add bounded retention plus session cleanup

**Approach:** Prune task state on disconnect/re-register, cap retained completed tasks per owner, and expire stale in-flight tasks after a configured ceiling.

**Pros:**
- Directly addresses leak/lifecycle issues
- Minimal architectural change

**Cons:**
- Requires careful policy choices for long-running tasks

**Effort:** 4-6 hours

**Risk:** Medium

---

### Option 2: Rework task indexing for list/read performance

**Approach:** Maintain per-owner indexes/queues and avoid full-map sort on every `tasks/list`.

**Pros:**
- Better scaling under polling
- Reduces allocations and lock hold time

**Cons:**
- Larger structural change
- More bookkeeping complexity

**Effort:** 6-10 hours

**Risk:** Medium

## Recommended Action

To be filled during triage.

## Technical Details

**Affected files:**
- [tasks.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/tasks.rs)
- [daemon.rs](/Users/robdezendorf/Documents/GitHub/plug/plug/src/daemon.rs)

**Related components:**
- Task retention policy
- Session disconnect cleanup
- Daemon admission control

## Resources

- **Branch:** `feat/core-mcp-tasks-support`
- **Related learning:** [2026-03-18-explicit-upstream-retirement-and-bounded-shutdown.md](/Users/robdezendorf/Documents/GitHub/plug/docs/solutions/integration-issues/2026-03-18-explicit-upstream-retirement-and-bounded-shutdown.md)

## Acceptance Criteria

- [ ] Completed task retention is bounded by policy, not only opportunistic access
- [ ] Stale in-flight tasks do not survive forever after client disconnect
- [ ] `tasks/list` performance does not degrade linearly with historical task volume without documented limits
- [ ] Daemon admission behavior is reviewed for unregistered-socket pressure

## Work Log

### 2026-03-22 - Code review capture

**By:** Codex / ce:review

**Actions:**
- Reviewed task retention and pruning logic in [tasks.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/tasks.rs)
- Cross-checked performance review findings on list/scanning behavior
- Captured lifecycle and scaling risks as follow-up hardening work

**Learnings:**
- Task correctness shipped first, but retention and indexing policy are now the next pressure point
- Disconnect cleanup and bounded retention should be treated as part of task lifecycle, not optional polish

