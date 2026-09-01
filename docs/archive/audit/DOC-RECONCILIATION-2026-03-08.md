# Document Reconciliation

This report classifies the major documentation sources against `main` truth.

## `docs/PLAN.md`

- state described: `main truth`
- factual status: accurate
- notes: current baseline is clear, remaining work is appropriately narrowed to Stream B + smaller follow-ups
- action: keep as the primary current-state doc

## `docs/ROADMAP-AUDIT-2026-03-08.md`

- state described: `main truth`
- factual status: accurate
- notes: strongest current evidence ledger; useful as audit backing doc rather than the casual starting point
- action: keep; continue updating only after merged roadmap-affecting PRs

## `docs/plans/2026-03-07-feat-mcp-spec-compliance-roadmap-plan.md`

- state described: mixed `plan / intended` plus explicit current-status sections
- factual status: mostly accurate after PR #31 updates
- notes: still a roadmap plan, not a simple truth doc; readers can confuse planned phases with shipped work if they skim
- action: keep, but always defer current-state questions to `docs/PLAN.md` and the audit snapshot

## `docs/plans/2026-03-07-feat-roadmap-tail-closeout-plan.md`

- state described: historical plan with completion status
- factual status: acceptable as historical record
- notes: should not be read as the current project overview
- action: mark or treat as historical planning context only

## `docs/plans/2026-03-08-feat-roots-forwarding-plan.md`

- state described: branch-scoped intended work
- factual status: accurate for off-main branch state, not for `main`
- notes: this must stay clearly branch-scoped until PR #32 merges
- action: keep branch-scoped framing

## `docs/RISKS.md`

- state described: current risks and drift concerns
- factual status: directionally accurate
- notes: risk section about documentation drift is still valid and should remain
- action: update after this reconciliation lands

## `CLAUDE.md`

- state described: current source of truth
- factual status: stale / misleading
- notes:
  - still says notification forwarding is incomplete
  - still says cancellation/progress passthrough is incomplete
  - still says full resources/prompts forwarding is incomplete
  - still lists `rmcp` as `1.0.0`
  - still mentions TUI-era dependencies remain in manifests, which is no longer true
- action: rewrite or replace with a truth-aligned version immediately

## Older phase plans under `docs/plans/2026-03-03-*` through `2026-03-07-*`

- state described: historical intended work
- factual status: mixed if read as current state
- notes: they are valuable as design history but are unsafe as casual truth sources
- action: keep as historical planning docs, but ensure the repo has one stronger canonical current-state doc

## `todos/*.md`

- state described: issue-level tracked work
- factual status: mostly accurate after the recent truth pass
- notes: todo names are useful evidence for issue lifecycle, but not sufficient to describe current product state
- action: keep as issue tracking only

## Reconciliation Summary

- current-state truth docs:
  - `docs/PLAN.md`
  - `docs/ROADMAP-AUDIT-2026-03-08.md`
- docs needing correction:
  - `CLAUDE.md`
- docs that must remain clearly non-truth:
  - most `docs/plans/*.md`
  - branch-scoped plans like roots forwarding until merged
