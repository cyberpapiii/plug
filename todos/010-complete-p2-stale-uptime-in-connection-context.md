---
status: complete
priority: p2
issue_id: "010"
tags: [code-review, correctness]
dependencies: []
---

# ConnectionContext.uptime_secs is stale

## Problem Statement

`uptime_secs` is captured once at connection time and never updated. For long-lived connections, Status queries return stale uptime.

## Findings

- **Source**: simplicity-review, performance-review, architecture-review
- **Location**: `daemon.rs:300-301`
- **Fix**: Store `started_at: Instant` instead, compute dynamically

## Acceptance Criteria

- [ ] Uptime is computed dynamically on each Status request
- [ ] `ConnectionContext` stores `started_at: Instant` instead of `uptime_secs: u64`
