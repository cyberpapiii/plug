---
status: complete
priority: p2
issue_id: "034"
tags: [code-review, docs, config]
dependencies: []
---

# Clarify legacy prefix config surface

## Problem Statement

`enable_prefix` is kept only for compatibility in `v0.1`, but some docs and reload semantics still treated it as an active setting.

## Findings

- `README.md` advertised `enable_prefix` as a live config switch
- `docs/PLAN.md` referenced stale `ready` files
- `plug-core/src/reload.rs` still marked `enable_prefix` as a meaningful restart boundary

## Recommended Action

Make the current state explicit:
- `enable_prefix` is legacy/ignored in `v0.1`
- README and plan docs should say so
- reload should stop treating it as an active semantic switch

## Acceptance Criteria

- [x] README no longer advertises prefix disabling as a live v0.1 feature
- [x] Plan references point to current completed tracking files
- [x] Reload semantics no longer include the inert `enable_prefix` field

## Work Log

### 2026-03-06 - Completed During Review Follow-up

**By:** Codex

**Actions:**
- Updated `README.md`
- Updated `docs/PLAN.md`
- Removed `enable_prefix` from restart-required handling in `plug-core/src/reload.rs`

**Learnings:**
- Keeping a compatibility field is fine, but the control plane and docs must stop pretending it still changes behavior.
