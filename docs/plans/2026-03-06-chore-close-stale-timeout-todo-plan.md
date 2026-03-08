---
title: "chore: close stale timeout todo"
type: chore
status: active
date: 2026-03-06
origin: docs/brainstorms/2026-03-06-timeout-semantics-brainstorm.md
---

# Close stale timeout todo

> Historical planning note: This todo-closeout plan is historical workflow context, not a current
> project-state reference. Use `todos/` plus `docs/PROJECT-STATE-SNAPSHOT.md` for current status.

## Overview

Todo `022` claims startup and tool calls share a single timeout. Current code shows this is already split and verified.

## Problem Statement / Motivation

The backlog should reflect reality. Keeping solved issues in `pending` status wastes execution time and obscures what work remains.

## Proposed Solution

- Verify the split in code and tests
- Mark todo `022` complete with a work-log entry
- Continue to the next unresolved todo

## Acceptance Criteria

- [ ] Code paths for startup and tool-call timeout are verified
- [ ] Todo `022` is renamed to `complete`
- [ ] Work log explains why the todo was stale

## Sources & References

- **Origin brainstorm:** `docs/brainstorms/2026-03-06-timeout-semantics-brainstorm.md`
- `plug-core/src/config/mod.rs`
- `plug-core/src/server/mod.rs`
- `plug-core/src/proxy/mod.rs`
