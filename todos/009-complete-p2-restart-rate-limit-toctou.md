---
status: complete
priority: p2
issue_id: "009"
tags: [code-review, security, concurrency]
dependencies: []
---

# TOCTOU race in restart_server rate-limit check

## Problem Statement

The rate-limit check and timestamp update are two separate DashMap operations. Two concurrent restart requests can both pass the check.

## Findings

- **Source**: security-review
- **Location**: `plug-core/src/engine.rs:244-265`
- **Fix**: Use DashMap's `entry()` API to make check-and-update atomic

## Acceptance Criteria

- [ ] Rate-limit check and timestamp update are atomic
- [ ] Uses DashMap entry API
