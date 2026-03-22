---
status: pending
priority: p2
issue_id: "067"
tags: [code-review, tasks, passthrough, resilience, correctness]
dependencies: []
---

# Cache terminal passthrough task results locally

## Problem Statement

For task-native upstream passthrough, `plug` currently re-fetches `tasks/result` from the upstream every time and does not persist the terminal payload in `TaskStore`.

If the upstream disappears or restarts after completion, the downstream task record can still exist while its result becomes unretrievable.

## Findings

- `get_task_result_for_owner()` fetches passthrough payloads from upstream directly in [mod.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/proxy/mod.rs).
- `sync_from_upstream_for_owner()` only synchronizes metadata, not payloads, in [tasks.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/tasks.rs).
- Multiple review agents independently flagged this as a resilience/correctness gap.

## Proposed Solutions

### Option 1: Cache payload on first successful `tasks/result`

**Approach:** Once a passthrough task reaches a terminal success state and the payload is retrieved, store it locally and serve subsequent reads from the task store.

**Pros:**
- Makes completed tasks resilient to upstream restart
- Smallest change from current model

**Cons:**
- Keeps more payload data in memory unless retention policy is tightened

**Effort:** 2-4 hours

**Risk:** Low

---

### Option 2: Cache payload as part of terminal status sync

**Approach:** Extend pass-through flow so terminal task info/result sync atomically updates both task metadata and payload in the local store.

**Pros:**
- Cleaner terminal-state model
- Better alignment between status and result durability

**Cons:**
- Slightly broader change surface

**Effort:** 4-6 hours

**Risk:** Medium

## Recommended Action

To be filled during triage.

## Technical Details

**Affected files:**
- [mod.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/proxy/mod.rs)
- [tasks.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/tasks.rs)

**Related components:**
- Passthrough task result retrieval
- Local task durability semantics

## Resources

- **Branch:** `feat/core-mcp-tasks-support`
- **Related learning:** [2026-03-18-reload-topology-background-tasks.md](/Users/robdezendorf/Documents/GitHub/plug/docs/solutions/integration-issues/2026-03-18-reload-topology-background-tasks.md)

## Acceptance Criteria

- [ ] Once a passthrough task result has been observed successfully, it remains readable from the local runtime even if the upstream disconnects
- [ ] Local cached result does not regress correctness for non-terminal or failed tasks
- [ ] Tests cover completed passthrough task read-after-upstream-loss behavior

## Work Log

### 2026-03-22 - Code review capture

**By:** Codex / ce:review

**Actions:**
- Reviewed passthrough result retrieval path in [mod.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/proxy/mod.rs)
- Confirmed payload durability is not currently part of the local task record
- Captured as an important resilience follow-up

**Learnings:**
- Passthrough mode currently provides status continuity better than result continuity
- Terminal payload durability needs to be made explicit if reconnect/runtime guarantees are important

