---
status: pending
priority: p2
issue_id: "006"
tags: [code-review, simplicity]
dependencies: []
---

# Remove Sub-phase C placeholder code

## Problem Statement

Several TUI features are placeholder stubs: ToolDetail mode (renders same as Tools), Logs mode (renders "coming in Sub-phase C"), setup_file_logging (dead code), HALF_OPEN_SYMBOL (unused), ConfigReloaded event (no visual effect).

## Findings

- **Source**: simplicity-review
- **Estimated LOC reduction**: ~65 lines
- **Locations**:
  - `app.rs:27` — ToolDetail variant + handlers
  - `widgets/logs.rs` — entire stub file
  - `daemon.rs:548-567` — setup_file_logging (#[allow(dead_code)])
  - `theme.rs:12` — HALF_OPEN_SYMBOL (#[allow(dead_code)])

## Acceptance Criteria

- [ ] No `#[allow(dead_code)]` annotations remain
- [ ] All placeholder modes removed
- [ ] Tests updated to reflect removed modes
