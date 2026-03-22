---
status: complete
priority: p1
issue_id: "065"
tags: [code-review, tasks, passthrough, correctness, side-effects]
dependencies: []
---

# Fail closed when a task-capable upstream returns a non-task response

## Problem Statement

When `plug` sends a task-wrapped tool call to an upstream that advertises task support, it only handles `CreateTaskResult`. If the upstream returns any other successful response shape, the code silently falls back to local wrapper-mode execution.

That can double-run the same tool call and duplicate side effects.

## Findings

- The pass-through branch is selected based on advertised upstream task capability in [mod.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/proxy/mod.rs).
- After sending the upstream task request, non-`CreateTaskResult` responses fall through to local execution instead of returning an error.
- Multiple review passes flagged this as a real correctness risk rather than a style issue.

## Proposed Solutions

### Option 1: Fail closed after upstream task dispatch

**Approach:** If an upstream advertises `tasks.requests.tools.call`, require `CreateTaskResult`; treat any other response as an error and do not execute locally.

**Pros:**
- Prevents duplicate side effects
- Keeps provenance unambiguous
- Smallest safe fix

**Cons:**
- Misbehaving upstreams will fail loudly instead of “sort of working”

**Effort:** 1-2 hours

**Risk:** Low

---

### Option 2: Add explicit compatibility fallback gating

**Approach:** Only allow wrapper fallback for a known allowlist of upstream quirks or older protocol combinations.

**Pros:**
- Can preserve compatibility with specific broken upstreams

**Cons:**
- Harder to reason about
- Adds policy complexity to a correctness path

**Effort:** 3-5 hours

**Risk:** Medium

## Recommended Action

To be filled during triage.

## Technical Details

**Affected files:**
- [mod.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/proxy/mod.rs)

**Related components:**
- Upstream task pass-through
- Wrapper-mode fallback
- Side-effecting tool calls

## Resources

- **Branch:** `feat/core-mcp-tasks-support`
- **Commits:** `6571950`, `476dac9`

## Acceptance Criteria

- [ ] A task-capable upstream never triggers local wrapper execution after a successful upstream task request has been sent
- [ ] Non-`CreateTaskResult` responses from task-capable upstreams fail the request explicitly
- [ ] Regression tests cover the non-task-response path

## Work Log

### 2026-03-22 - Code review capture

**By:** Codex / ce:review

**Actions:**
- Inspected pass-through task dispatch branch in [mod.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/proxy/mod.rs)
- Compared agent findings and confirmed the fallback path can duplicate execution
- Logged as a merge-blocking issue

**Learnings:**
- Compatibility fallbacks are dangerous once a side-effecting request has already been sent upstream
- Task pass-through needs fail-closed semantics, not best-effort fallback

### 2026-03-22 - Fix implemented

**By:** Codex / ce:work

**Actions:**
- Changed task-capable upstream dispatch in [mod.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/proxy/mod.rs) to error on any non-`CreateTaskResult` response after upstream task dispatch
- Removed the possibility of silently falling back to local wrapper execution after an upstream task request has already been sent

**Learnings:**
- Once side effects may already have happened upstream, the only safe fallback is no fallback
