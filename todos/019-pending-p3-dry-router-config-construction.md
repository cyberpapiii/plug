---
status: pending
priority: p3
issue_id: "019"
tags: [code-review, simplicity]
dependencies: []
---

# RouterConfig construction duplicated in main.rs and engine.rs

## Problem Statement

Both `router_config()` in main.rs and the Engine constructor manually copy 5 fields from Config to RouterConfig. Add `impl From<&Config> for RouterConfig`.

## Findings

- **Source**: simplicity-review
- **Locations**: `main.rs:184-193`, `engine.rs:124-130`

## Acceptance Criteria

- [ ] Single `From<&Config> for RouterConfig` implementation
- [ ] Both call sites use `RouterConfig::from(&config)`
