---
status: complete
priority: p2
issue_id: "014"
tags: [code-review, architecture, ux]
dependencies: []
---

# cmd_status fallback starts all upstream servers

## Problem Statement

When no daemon is running, `plug status` starts ALL upstream servers just to query their health, then shuts them down. This is unexpectedly heavyweight (~10s), launches child processes, and may have side effects.

## Findings

- **Source**: architecture-review
- **Location**: `plug/src/main.rs:279-350`

## Proposed Solutions

Show config-only status (server names, transport, enabled) when no daemon is running. Display `source: config` vs `source: daemon`.
- **Effort**: Small

## Acceptance Criteria

- [ ] `plug status` without daemon shows config-only info (no server startup)
- [ ] Output indicates whether data is from daemon or config
