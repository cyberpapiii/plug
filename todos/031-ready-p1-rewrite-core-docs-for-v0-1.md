---
status: ready
priority: p1
issue_id: "031"
tags: [v0-1, docs, architecture, roadmap]
dependencies: ["029", "030"]
---

# Rewrite core docs to match the `v0.1` product

## Problem Statement

The core docs still mix `fanout`, TUI-first stories, pre-development language, and stale dependency/version information with a codebase that is already a daemon-backed CLI MCP multiplexer.

## Findings

- Major drift documented in `docs/plans/2026-03-06-strategic-assessment.md`
- `ARCHITECTURE.md`, `PLAN.md`, `CRATE-STACK.md`, `RISKS.md`, and `CLAUDE.md` are the highest-value fixes
- Product story should be CLI-first and daemon-centered
- TUI should not be described as implemented

## Proposed Solutions

### Option 1: Rewrite the core docs now (Recommended)

**Approach:** Update the small set of top-level truth docs before any Phase 2 feature work.

**Pros:**
- Gives the repo one truthful narrative
- Unblocks future planning and execution

**Cons:**
- Requires deliberate writing work

**Effort:** Medium

**Risk:** Low

## Recommended Action

Rewrite the core truth docs using the strategic assessment and `v0.1` execution plan as source material. Keep them concise and operationally accurate.

## Acceptance Criteria

- [ ] `ARCHITECTURE.md` describes `plug`, not `fanout`
- [ ] `PLAN.md` no longer presents the old 5-phase checkbox roadmap as current truth
- [ ] `CRATE-STACK.md` reflects actual dependencies and versions
- [ ] `RISKS.md` reflects current post-stabilization risks
- [ ] `CLAUDE.md` matches the actual codebase and product posture

## Work Log

### 2026-03-06 - Created During v0.1 Task Planning

**By:** Codex

**Actions:**
- Created after stabilization code tasks reached a clean checkpoint

**Learnings:**
- This is the phase gate before Phase 2, not optional polish
