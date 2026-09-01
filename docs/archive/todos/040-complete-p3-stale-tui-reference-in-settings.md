---
status: complete
priority: p3
issue_id: "040"
tags: [code-review, documentation, quality]
dependencies: []
---

# Stale TUI reference in compound-engineering.local.md

## Problem Statement

`compound-engineering.local.md` line 10 still says "TUI uses ratatui 0.30 + crossterm 0.29" but those dependencies were removed in PR #29. This gives review agents incorrect context.

## Findings

- `compound-engineering.local.md:10`: `- TUI uses ratatui 0.30 + crossterm 0.29`
- PR #29 removed ratatui, crossterm, and color-eyre from workspace manifest
- Roadmap tail plan acceptance criterion: "no touched docs imply router/runtime reload behavior that the code does not implement"

## Proposed Solutions

Remove or update the TUI line in compound-engineering.local.md.

**Effort:** Small
**Risk:** None

## Acceptance Criteria

- [ ] compound-engineering.local.md does not reference removed TUI dependencies

## Work Log

- 2026-03-07: Identified during PR #30 review
