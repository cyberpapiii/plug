---
status: pending
priority: p2
issue_id: "008"
tags: [code-review, security, correctness]
dependencies: []
---

# write_frame does not check outgoing frame size

## Problem Statement

`write_frame` casts `payload.len() as u32` without bounds check. If payload > 4GB (theoretically possible with many tools), the cast silently truncates, causing the client to read a partial frame and corrupt the session.

## Findings

- **Source**: security-review, simplicity-review
- **Location**: `daemon.rs:193-202`

## Proposed Solutions

Add `u32::try_from(payload.len())` check before sending, matching `read_frame`'s MAX_FRAME_SIZE check.
- **Effort**: Small (5 min)

## Acceptance Criteria

- [ ] `write_frame` rejects payloads > MAX_FRAME_SIZE
- [ ] Uses `u32::try_from` instead of `as u32`
