---
status: complete
priority: p2
issue_id: "046"
tags: [code-review, quality, simplification]
dependencies: []
---

# Extract DaemonBridge send-and-await helper to reduce duplication

## Problem Statement

`DaemonBridge::create_elicitation` and `DaemonBridge::create_message` share ~30 lines of identical channel-send-response-match logic. Each method is ~43 lines with the capability gate + channel send + oneshot await + response variant match. Only the capability check, `IpcClientRequest` variant, and `IpcClientResponse` variant differ.

Flagged by: code-simplicity-reviewer.

## Findings

- `plug/src/daemon.rs:546-632`: Two methods with ~60 lines of identical structure out of 86 total
- Shared pattern: clone tx + session_id, create oneshot, send on channel, await response, match variant

## Proposed Solutions

### Option A: Extract `send_and_await` helper (Recommended)

```rust
async fn send_and_await(&self, request: IpcClientRequest) -> Result<IpcClientResponse, McpError> {
    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    self.reverse_request_tx.send((request, resp_tx)).await
        .map_err(|_| McpError::internal_error(...))?;
    resp_rx.await
        .map_err(|_| McpError::internal_error(...))
}
```

Each method reduces to: capability check + call helper + unwrap variant.

- Pros: ~25 LOC saved, clearer intent
- Cons: Minor indirection
- Effort: Small
- Risk: None

## Recommended Action

_To be filled during triage_

## Technical Details

- **Affected files**: `plug/src/daemon.rs`

## Acceptance Criteria

- [ ] Common channel-send-await logic extracted into helper
- [ ] Both bridge methods use the helper
- [ ] Existing unit tests still pass

## Work Log

| Date | Action | Learnings |
|------|--------|-----------|
| 2026-03-08 | Created from CE review | Code simplicity reviewer identified as strongest duplication candidate |
