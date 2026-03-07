---
status: complete
priority: p1
issue_id: "029"
tags: [v0-1, cli, runtime, truthfulness]
dependencies: []
---

# Make `plug serve --stdio` truthful

## Problem Statement

The CLI accepts `plug serve --stdio`, but the implementation ignores the flag and always starts the HTTP server. This is misleading product surface area in a stabilization release.

## Findings

- Flag declared in `plug/src/main.rs`
- `_stdio` ignored in `plug/src/runtime.rs`
- This is a product-truth bug, not a transport feature gap

## Proposed Solutions

### Option 1: Remove or hide `--stdio` for v0.1 (Recommended)

**Approach:** Keep `serve` as the HTTP/background serve path and stop advertising an unimplemented stdio mode.

**Pros:**
- Smallest, honest fix
- Avoids inventing another transport path during stabilization

**Cons:**
- Slight CLI surface change

**Effort:** Small

**Risk:** Low

### Option 2: Implement stdio serve mode

**Approach:** Add a real stdio-serving code path under `serve`.

**Pros:**
- Matches current flag surface

**Cons:**
- Expands scope beyond stabilization
- Overlaps with existing `connect` behavior

**Effort:** Medium

**Risk:** Medium

## Recommended Action

Hide or remove the misleading `--stdio` flag for `v0.1`, update help text, and add a regression test or command-surface assertion if practical.

## Acceptance Criteria

- [x] `plug serve --stdio` is no longer misleadingly exposed, or it errors explicitly as unsupported
- [x] CLI help output matches actual behavior
- [x] No regression in standard `plug serve` behavior

## Work Log

### 2026-03-06 - Created During v0.1 Task Planning

**By:** Codex

**Actions:**
- Derived from runtime review after crash recovery and stabilization planning

**Learnings:**
- Small surface-truth issues compound quickly in a CLI-first product

### 2026-03-06 - Completed In Worktree Execution

**By:** Codex

**Actions:**
- Verified the misleading `--stdio` flag had already been removed from the current command surface
- Added CLI parse regression tests in `plug/src/main.rs`
- Verified the command surface with focused `cargo test -p plug serve_command` and `cargo check`

**Learnings:**
- Because the worktree branched from an already-stabilized `main`, this task was partly complete on arrival; the remaining value was locking it in with tests and tracked completion.
