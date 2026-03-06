---
status: ready
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

- [ ] `plug serve --stdio` is no longer misleadingly exposed, or it errors explicitly as unsupported
- [ ] CLI help output matches actual behavior
- [ ] No regression in standard `plug serve` behavior

## Work Log

### 2026-03-06 - Created During v0.1 Task Planning

**By:** Codex

**Actions:**
- Derived from runtime review after crash recovery and stabilization planning

**Learnings:**
- Small surface-truth issues compound quickly in a CLI-first product
