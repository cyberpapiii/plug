---
status: complete
priority: p1
issue_id: "001"
tags: [code-review, correctness, tui]
dependencies: []
---

# ToolCallCompleted updates wrong activity entry

## Problem Statement

When `ToolCallCompleted` arrives, the code assumes the matching entry is at the front of the `VecDeque` (`activity_log.front_mut()`), but interleaved events can push it deeper. If calls A and B start, and B completes before A, B's completion updates A's entry.

## Findings

- **Source**: architecture-review, performance-review, simplicity-review (all flagged independently)
- **Location**: `plug/src/tui/app.rs:504-519`
- **Evidence**: `in_flight` HashMap stores index `0` for every entry, then always updates `front_mut()` regardless of actual position

## Proposed Solutions

### Option A: Store call_id in ActivityEntry (Recommended)
Add `call_id: Option<u64>` to `ActivityEntry`, search with `iter_mut().find()` on completion.
- **Pros**: Simple, correct, preserves existing log structure
- **Cons**: O(n) search on completion (n ≤ 1000)
- **Effort**: Small

### Option B: Push completions as separate entries
Remove `in_flight` entirely, push each `ToolCallCompleted` as a new `ActivityEntry`.
- **Pros**: Simplest, eliminates correlation entirely
- **Cons**: Duplicates tool call info in log
- **Effort**: Small

## Acceptance Criteria

- [ ] Concurrent tool calls are correctly correlated in the activity log
- [ ] `in_flight` HashMap either works correctly or is removed
- [ ] Tests verify interleaved call completion updates the right entry
