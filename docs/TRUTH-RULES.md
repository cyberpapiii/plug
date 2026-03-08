# Truth Rules

These rules define how project status must be described.

Compound Engineering (CE) remains the workflow operating system for this repo. These rules are the
repo-local truth and drift-prevention layer on top of CE, not a replacement for CE.

## Current State Rules

- `main` is the only source of truth for "implemented now"
- code on a branch or worktree is never "done" until it exists on `main`
- a merged PR counts only if the merged code is present on current `main`

## Documentation Rules

- docs are claims, not evidence
- every current-state claim must be supportable by code and, where relevant, tests
- plans describe intended work, not current truth
- branch-scoped docs must explicitly say they describe branch state
- historical docs must not be presented as current-state references

## Agent Rules

- start with `docs/PROJECT-STATE-SNAPSHOT.md` for any progress or roadmap question
- use `main` as the only source of “done now”
- subagents are encouraged for bounded evidence gathering and review work
- subagents do not decide final truth; the main thread does
- agent outputs and PR summaries are leads, not evidence

## Required Labels

Every roadmap-relevant feature should be described with one of:

- `done on main`
- `partial on main`
- `exists off-main`
- `missing`

## PR Truth Pass

Every roadmap-affecting PR should complete this checklist after merge:

- [ ] merged code exists on `main`
- [ ] `docs/PLAN.md` still matches `main`
- [ ] `docs/PROJECT-STATE-SNAPSHOT.md` still matches `main`
- [ ] any branch-only wording is removed or relabeled
- [ ] remaining-work lists are revalidated

## Canonical Docs

Use these docs in this order:

1. `docs/PROJECT-STATE-SNAPSHOT.md`
2. `docs/PLAN.md`
3. `docs/ROADMAP-AUDIT-2026-03-08.md`
4. `docs/audit/*.md`
5. `AGENTS.md` / `CLAUDE.md` for repo-local workflow enforcement
