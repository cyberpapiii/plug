---
status: pending
priority: p2
issue_id: "015"
tags: [code-review, correctness, tui]
dependencies: []
---

# full_refresh() does not reset in_flight, clients, or tools

## Problem Statement

After a `Lagged` recovery, `full_refresh()` only replaces `servers` and `tool_count`. The `in_flight` map retains orphaned entries, `clients` list becomes stale, and `tools` vector is never populated.

## Findings

- **Source**: architecture-review
- **Location**: `plug/src/tui/app.rs:169`

## Acceptance Criteria

- [ ] `full_refresh()` clears `in_flight`
- [ ] `full_refresh()` refreshes `tools` from snapshot
- [ ] `EngineSnapshot` expanded to include client data (or client count)
