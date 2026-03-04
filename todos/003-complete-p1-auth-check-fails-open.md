---
status: complete
priority: p1
issue_id: "003"
tags: [code-review, security]
dependencies: []
---

# Auth check silently passes when token extraction returns None

## Problem Statement

The auth check in `handle_ipc_connection` uses a nested `if let Some` guard. If `extract_auth_token` returns `None` for a mutating command, the request falls through without rejection — failing open.

## Findings

- **Source**: security-review
- **Location**: `plug/src/daemon.rs:378-389`
- **Evidence**: Currently safe because all mutating variants have mandatory `auth_token` field, but structurally fragile for future additions

## Proposed Solutions

### Option A: Fail closed with match (Recommended)
Replace `if let Some` with `match` that explicitly rejects `None`.
- **Effort**: Small (10 min)

## Acceptance Criteria

- [ ] Auth check rejects requests when token is missing (fails closed)
- [ ] Test verifies a mutating request without token is rejected
