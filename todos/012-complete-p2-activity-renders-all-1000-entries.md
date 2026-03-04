---
status: complete
priority: p2
issue_id: "012"
tags: [code-review, performance]
dependencies: []
---

# Activity panel renders all 1000 entries per frame

## Problem Statement

The activity widget builds `Vec<ListItem>` from all 1000 entries on every render, even though only ~20-30 are visible on screen. This is 4000 ListItem constructions/sec at idle.

## Findings

- **Source**: performance-review
- **Location**: `plug/src/tui/widgets/activity.rs:26-57`
- **Impact**: Largest performance concern in TUI rendering path

## Proposed Solutions

Limit rendered items to `area.height` entries around current selection.
- **Effort**: Small

## Acceptance Criteria

- [ ] Only visible entries are rendered per frame
- [ ] Scrolling still works correctly
