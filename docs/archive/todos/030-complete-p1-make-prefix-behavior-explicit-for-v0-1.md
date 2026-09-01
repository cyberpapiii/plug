---
status: complete
priority: p1
issue_id: "030"
tags: [v0-1, config, proxy, docs]
dependencies: []
---

# Make tool prefix behavior explicit for `v0.1`

## Problem Statement

`enable_prefix` exists in config and some diagnostics, but the router always prefixes tool names. This creates a false impression that prefix-off mode is supported.

## Findings

- Config field exists in `plug-core/src/config/mod.rs`
- Routing behavior always prefixes in `plug-core/src/proxy/mod.rs`
- Doctor messaging still implies prefix-off support
- The `v0.1` execution plan intentionally rejected reviving prefix-off mode during stabilization

## Proposed Solutions

### Option 1: Deprecate/ignore `enable_prefix` for `v0.1` (Recommended)

**Approach:** Keep prefixing as the only supported behavior for now, and make config/docs/doctor output explicit about that.

**Pros:**
- Honest
- Small scope
- Avoids collision/routing ambiguity during stabilization

**Cons:**
- Leaves a legacy config field in place temporarily

**Effort:** Small

**Risk:** Low

### Option 2: Fully support prefix-off mode now

**Approach:** Wire `enable_prefix` into the router and define collision behavior.

**Pros:**
- Config matches runtime

**Cons:**
- Not a stabilization fix
- Reintroduces routing ambiguity

**Effort:** Medium

**Risk:** Medium

## Recommended Action

Treat prefixing as always-on for `v0.1`, make `enable_prefix` explicit legacy/no-op behavior, and update docs accordingly.

## Acceptance Criteria

- [x] Runtime/docs/doctor agree on current prefix behavior
- [x] No claim remains that prefix-off mode is supported in `v0.1`
- [x] Existing tool routing behavior is unchanged

## Work Log

### 2026-03-06 - Created During v0.1 Task Planning

**By:** Codex

**Actions:**
- Split out from stabilization execution plan after reload/runtime work completed

**Learnings:**
- This is better treated as an honesty/docs task than as a router feature task

### 2026-03-06 - Completed In Worktree Execution

**By:** Codex

**Actions:**
- Updated `Config` field documentation to mark `enable_prefix` as legacy/ignored in `v0.1`
- Changed doctor output to report prefixing as always-on behavior instead of warning about a nonexistent prefix-off mode
- Updated stale architecture/plan language to match actual runtime behavior
- Verified with focused `cargo test -p plug-core doctor` and `cargo check`

**Learnings:**
- Treating this as a truth-alignment task avoided unnecessary router work during stabilization.
