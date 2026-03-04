---
status: complete
priority: p1
issue_id: "004"
tags: [code-review, security]
dependencies: []
---

# Auth token file briefly world-readable between write and chmod

## Problem Statement

`std::fs::write` creates the token file with umask-default permissions (typically 0644). There's a window between `write()` and `set_permissions(0600)` where the file is world-readable.

## Findings

- **Source**: security-review
- **Location**: `plug/src/daemon.rs:228-235`
- **Evidence**: Two-step create-then-chmod pattern has TOCTOU window

## Proposed Solutions

### Option A: Use OpenOptions with mode (Recommended)
Use `OpenOptions::new().write(true).create_new(true).mode(0o600)` to set permissions at creation.
- **Effort**: Small (15 min)

## Acceptance Criteria

- [ ] Token file is created with 0600 from the first byte
- [ ] Same fix applied to PID file creation
