---
status: complete
priority: p2
issue_id: "016"
tags: [code-review, security]
dependencies: []
---

# ensure_dir has permissions race on newly created directories

## Problem Statement

`create_dir_all` creates with umask-default permissions, then `set_permissions` restricts. Use `DirBuilder::new().mode(0o700).recursive(true)` instead.

## Findings

- **Source**: security-review
- **Location**: `daemon.rs:114-126`
- **Effort**: Small (10 min)

## Acceptance Criteria

- [ ] Uses `DirBuilder` with mode 0o700 for all directory creation
