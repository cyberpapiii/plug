---
status: complete
priority: p1
issue_id: "044"
tags: [code-review, architecture, ipc, compatibility]
dependencies: []
---

# IPC_PROTOCOL_VERSION not bumped after envelope format change

## Problem Statement

The IPC protocol version remains at `2`, even though the wire format has materially changed. During tool calls with reverse requests, the daemon now sends `DaemonToProxyMessage` envelope frames (tagged with `"envelope"`) intermixed with plain `IpcResponse` frames (tagged with `"type"`).

A new daemon sending envelope frames to an old proxy client (still at protocol v2) will cause parse failures mid-tool-call. The proxy and daemon already enforce strict version matching during `Register`, so bumping to v3 would produce a clear error message ("daemon supports IPC protocol v3, got v2") rather than a mysterious mid-call JSON parse failure.

The backward-compatibility fallback in `ipc_proxy.rs` handles the old-daemon/new-proxy direction, but not new-daemon/old-proxy.

Flagged by: architecture-strategist.

## Findings

- `plug-core/src/ipc.rs`: `IPC_PROTOCOL_VERSION` remains at `2`
- `plug/src/ipc_proxy.rs:143-173`: Fallback parse handles old daemon -> new proxy
- `plug/src/daemon.rs:928-983`: New daemon sends envelope frames that old proxy cannot parse
- Version check during Register: strict equality, would produce clear error if bumped

## Proposed Solutions

### Option A: Bump to v3 (Recommended)

Change `IPC_PROTOCOL_VERSION` from `2` to `3` in `plug-core/src/ipc.rs`.

- Pros: Clear version mismatch error on rolling upgrade, correct semantic versioning of the protocol
- Cons: Forces simultaneous upgrade of daemon and proxy (but they ship together anyway)
- Effort: Trivial (1 line change)
- Risk: None

## Recommended Action

_To be filled during triage_

## Technical Details

- **Affected files**: `plug-core/src/ipc.rs`

## Acceptance Criteria

- [ ] `IPC_PROTOCOL_VERSION` is `3`
- [ ] Old proxy connecting to new daemon gets clear version mismatch error

## Work Log

| Date | Action | Learnings |
|------|--------|-----------|
| 2026-03-08 | Created from CE review | Architecture strategist flagged as must-fix |

## Resources

- IPC version check: `plug-core/src/ipc.rs`
