---
status: complete
priority: p3
issue_id: "035"
tags: [code-review, docs, export]
dependencies: []
---

# Clarify YAML HTTP export compatibility note

## Problem Statement

YAML export for Goose still uses `type: sse` even when targeting the active HTTP endpoint. Without explanation, that looked like an architectural contradiction.

## Findings

- `plug-core/src/export.rs` emits `type: sse` for Goose HTTP export
- `plug-core/src/import.rs` also expects Goose YAML to use `stdio` or `sse`

## Recommended Action

Clarify in architecture docs that some client-specific export formats still use legacy transport labels for compatibility even though the active server surface is Streamable HTTP.

## Acceptance Criteria

- [x] Architecture docs explain the compatibility distinction
- [x] No code-path change was required without stronger client-format evidence

## Work Log

### 2026-03-06 - Completed During Review Follow-up

**By:** Codex

**Actions:**
- Added a compatibility note to `docs/ARCHITECTURE.md` rather than changing client export code without hard evidence of a different Goose schema

**Learnings:**
- When a review finding is about apparent contradiction, a precise compatibility note can be the right fix if runtime behavior is intentional.
