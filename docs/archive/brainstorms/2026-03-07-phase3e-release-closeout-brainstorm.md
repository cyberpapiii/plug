# Brainstorm: Phase 3E Release Closeout and Truth Pass

**Date**: 2026-03-07
**Status**: Ready for planning

## What We're Building

The final planned Phase 3 tranche is a release-closeout pass that makes the tracked operating docs
match the code and merged roadmap reality after Phases 1-3.

This tranche focuses on:

- updating tracked docs that still describe `v0.1` as the active target
- updating the risk register to current post-Phase-3 risks
- collapsing the research breadcrumb list from pre-implementation open questions to current
  remaining questions

This tranche does **not** add new runtime behavior.

## Why This Approach

The code has moved much faster than the tracked top-level docs. Right now the repo’s most important
operator-facing documents still describe missing Phase 2/3 work that is already merged.

That is the last meaningful consistency gap before the `v0.2.0` boundary.

## Key Decisions

- **Treat this as a truth pass, not a speculative roadmap rewrite.**
- **Update only the tracked operating docs that a maintainer would actually consult.**
- **Prefer concise current-state summaries over leaving the old pre-implementation research lists in
  place unchanged.**
- **Do not tag `v0.2.0` inside this branch.**
  Prepare the repository for that boundary; tag after merge.

## Resolved Questions

- **Should this tranche add new runtime features?** No
- **Should the final release tag be created in-branch?** No
- **Should stale Phase 2/3 “future work” references be removed from tracked docs?** Yes

## Open Questions

None. The remaining work is editorial/operational and narrowly scoped.

## Next

Write a focused plan for:

1. updating `PLAN.md`
2. updating `RISKS.md`
3. rewriting `RESEARCH-BREADCRUMBS.md` to current unresolved items
4. correcting `ARCHITECTURE.md` / `CRATE-STACK.md` version and scope drift
