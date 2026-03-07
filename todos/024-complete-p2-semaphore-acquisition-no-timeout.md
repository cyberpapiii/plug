---
status: complete
priority: p2
issue_id: "024"
tags: [code-review, performance, reliability, issue-7]
dependencies: []
---

# Semaphore acquisition in call_tool has no timeout — can block indefinitely

## Problem Statement

`acquire_owned().await` at proxy/mod.rs:247 has no timeout. With `max_concurrent = 1` (default for stdio), a single slow tool call blocks every subsequent call to that server indefinitely. The `tokio::time::timeout` at line 291 only wraps `peer.call_tool()`, not the semaphore acquisition. The AI client sees no error, no timeout — just an infinite hang.

## Findings

- **Source**: performance-oracle
- **Location**: `plug-core/src/proxy/mod.rs:246-254`
- **Evidence**: `sem.clone().acquire_owned().await` — no `tokio::time::timeout` wrapper. The timeout at line 291 is applied after the semaphore is already acquired.

## Proposed Solutions

### Option A: Wrap semaphore acquisition in timeout (Recommended)
Add `tokio::time::timeout(Duration::from_secs(30), sem.acquire_owned())` with appropriate error.
- **Pros**: Prevents indefinite blocking, surfaces error to client
- **Cons**: Adds another timeout value to configure
- **Effort**: Small

### Option B: Move call timeout to encompass entire operation
Wrap the whole sequence (semaphore + call) in a single timeout.
- **Pros**: Single timeout covers everything, simpler mental model
- **Cons**: Harder to distinguish "waiting for semaphore" from "waiting for upstream"
- **Effort**: Medium

### Option C: Remove semaphore (YAGNI for v0.1.0)
Desktop tool with one user — concurrency limiting is premature optimization.
- **Pros**: Simplest, removes the problem entirely
- **Cons**: No protection against overwhelming a serial stdio server
- **Effort**: Small

## Acceptance Criteria

- [x] Semaphore acquisition does not block indefinitely
- [x] Client receives an error (not silence) when concurrency limit is reached
- [x] Error message indicates the cause (server overloaded / max concurrent reached)

## Work Log

### 2026-03-06 - Completed In Worktree Execution

**By:** Codex

**Actions:**
- Added a bounded semaphore acquisition timeout in `plug-core/src/proxy/mod.rs`
- Added a dedicated `ProtocolError::ServerBusy` in `plug-core/src/error.rs`
- Added a paused-time async test covering the overload path

**Learnings:**
- This fix was cleanly isolated to the tool-call hot path and did not require any broader runtime or config redesign.
