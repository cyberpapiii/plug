---
status: pending
priority: p2
issue_id: "011"
tags: [code-review, security]
dependencies: []
---

# IpcRequest Debug derives expose auth tokens in logs

## Problem Statement

`IpcRequest` derives `Debug`, which will print auth_token fields in plain text if logged. Should implement custom Debug that redacts tokens, matching the SecretString pattern already used in types.rs.

## Findings

- **Source**: security-review
- **Location**: `plug-core/src/ipc.rs:14-45`

## Acceptance Criteria

- [ ] Custom Debug impl on IpcRequest that prints `[REDACTED]` for auth_token fields
- [ ] Consistent with SecretString pattern in types.rs
