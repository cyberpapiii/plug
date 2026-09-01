---
status: complete
priority: p2
issue_id: "033"
tags: [code-review, docs, architecture, agent]
dependencies: []
---

# Clarify runtime sharing boundary in docs

## Problem Statement

The rewritten docs implied `plug connect` and `plug serve` shared a single live runtime, but `plug serve` actually starts its own engine unless daemon mode is used. That mismatch could mislead agents and humans about process isolation and duplicate upstream startup behavior.

## Findings

- `CLAUDE.md` and `docs/ARCHITECTURE.md` described one shared runtime
- `plug/src/runtime.rs` shows `plug serve` creates its own `Engine`

## Recommended Action

Rewrite the docs to describe the real boundary:
- shared daemon runtime for local stdio clients
- dedicated engine instance for `plug serve` unless daemon mode is used

## Acceptance Criteria

- [x] Top-level docs describe the real runtime boundary
- [x] No claim remains that `plug serve` automatically shares the daemon runtime

## Work Log

### 2026-03-06 - Completed During Review Follow-up

**By:** Codex

**Actions:**
- Updated `CLAUDE.md`
- Updated `docs/ARCHITECTURE.md`

**Learnings:**
- Small wording drift in architecture docs quickly becomes automation drift for agents.
