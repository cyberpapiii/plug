---
status: pending
priority: p3
issue_id: "017"
tags: [code-review, ux]
dependencies: []
---

# Search (/) in Dashboard mode has no visible effect

## Problem Statement

The `/` keybinding activates search from Dashboard mode, but `filtered_tools()` only filters `self.tools` which are not displayed in Dashboard. Search should only be active in Tools view.

## Findings

- **Source**: simplicity-review
- **Location**: `app.rs:372-376`

## Acceptance Criteria

- [ ] Search keybinding only activates in Tools view
- [ ] Or search in Dashboard filters servers/clients (if desired)
