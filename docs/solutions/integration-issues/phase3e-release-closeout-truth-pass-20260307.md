---
title: "Phase 3E release closeout required a truth pass across the tracked operating docs"
category: integration-issues
tags:
  - documentation
  - release-closeout
  - risk-register
  - roadmap
  - architecture
  - crate-stack
  - test-stability
module: docs + plug
date: 2026-03-07
symptom: |
  After the merged Phase 1-3 work, the top-level tracked docs still described the old `v0.1`
  checkpoint and multiple already-completed gaps as if they were future work. During the closeout
  pass, the standard verification gate also exposed that the merged daemon continuity test still had
  a cold-suite readiness flake even though the feature itself was fixed.
root_cause: |
  The implementation tranches moved faster than the tracked operating docs, so the repo’s
  maintainer-facing truth layer lagged behind the code. Separately, the daemon continuity test was
  correct in structure but still slightly too aggressive about startup timing under a fully cold
  suite, which made the final release-closeout pass catch a remaining nondeterminism.
severity: medium
related:
  - docs/brainstorms/2026-03-07-phase3e-release-closeout-brainstorm.md
  - docs/plans/2026-03-07-feat-phase3e-release-closeout-plan.md
  - docs/PLAN.md
  - docs/RISKS.md
  - docs/RESEARCH-BREADCRUMBS.md
  - docs/ARCHITECTURE.md
  - docs/CRATE-STACK.md
  - plug/src/ipc_proxy.rs
---

# Phase 3E release closeout required a truth pass across the tracked operating docs

## Problem

By the time Phase 3D was merged, the code and the repo’s tracked operational docs no longer agreed
about the project’s state.

Examples:

- `docs/PLAN.md` still described `v0.1` as the active target
- `docs/RISKS.md` still listed already-completed Phase 2/3 work as missing
- `docs/RESEARCH-BREADCRUMBS.md` was still a pre-implementation question dump instead of a current
  open-questions ledger
- `docs/ARCHITECTURE.md` and `docs/CRATE-STACK.md` still framed merged capabilities and dependency
  state as future work

The release-closeout pass also found one last stability issue: the daemon continuity test needed a
slightly wider startup-readiness window under a cold full-suite run.

## Solution

### 1. Rewrite the tracked operating docs to current reality

The closeout pass intentionally focused on the documents a maintainer would actually use:

- `docs/PLAN.md`
- `docs/RISKS.md`
- `docs/RESEARCH-BREADCRUMBS.md`
- `docs/ARCHITECTURE.md`
- `docs/CRATE-STACK.md`

The rule was simple: prefer concise current-state truth over preserving stale historical framing.

### 2. Reduce the research breadcrumb list to the real remaining questions

Instead of leaving the old long pre-coding list in place, the breadcrumb file now tracks only the
questions that are still genuinely open after the merged Phase 1-3 work.

That makes it useful again as a planning input instead of a historical artifact.

### 3. Stabilize the last known flaky release-gate test

The closeout gate caught that the merged daemon continuity test could still fail under a fully cold
suite because its initial startup-readiness window was too tight.

The fix was not a product change. It was a harness-tolerance change:

- widen the initial readiness deadline for the mock upstream inside the daemon continuity test

That keeps the release gate deterministic without weakening what the test actually proves.

## Verification

The closeout branch still ran the standard gate:

- `cargo test`
- `cargo clippy --all-targets --all-features -- -D warnings`

That mattered because this pass touched both docs and the release-gate test harness.

## Prevention / Reuse

The final lesson is that release-closeout should be treated as a real engineering tranche, not an
afterthought.

Two specific habits should carry forward:

1. top-level tracked docs need a final truth pass after major feature waves
2. the final verification gate should still run even on documentation-heavy branches, because it
   catches harness drift and last-mile nondeterminism

This tranche did not add a new product capability. It made the repository honest about the
capabilities that already exist.
