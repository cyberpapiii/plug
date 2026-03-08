# AGENTS.md — plug

This repo uses **Compound Engineering (CE)** as its workflow operating system.

This file does **not** replace CE. It explains how agents should use CE safely in this repo so
project-state tracking does not drift again.

## First Step For Any Progress / Roadmap / Status Question

Before answering anything about what is done, in progress, missing, or on a branch:

1. Read `docs/PROJECT-STATE-SNAPSHOT.md`
2. Read `docs/PLAN.md` if more detail is needed
3. Verify against code on `main` if the answer materially matters

Do not answer project-progress questions from plans, PR descriptions, branch summaries, or older
agent outputs alone.

## Truth Model

- `main` is the only source of “done now”
- branch or worktree code is never “done now” until merged to `main`
- merged PR summaries are not evidence unless current `main` contains the code
- docs are claims, not evidence

Use only these labels for roadmap-relevant features:

- `done on main`
- `partial on main`
- `exists off-main`
- `missing`

If unsure, prefer `exists off-main` or `missing`, never `done on main`.

## Compound Engineering Doc Roles

CE remains the operating system. In this repo, doc roles are:

- current truth:
  - `docs/PROJECT-STATE-SNAPSHOT.md`
  - `docs/PLAN.md`
  - `docs/TRUTH-RULES.md`
- evidence / audit backing:
  - `docs/ROADMAP-AUDIT-2026-03-08.md`
  - `docs/audit/*.md`
- intended work:
  - `docs/plans/*.md`
- historical / compound knowledge:
  - `docs/solutions/*.md`
  - `docs/research/*.md`
  - older phase plans
- tracked issues:
  - `todos/*.md`

Plans are not current truth. Historical docs are not current truth.

## Repo-Specific Gotchas

- There are many active worktrees. Do not confuse worktree state with `main`.
- `feat/roots-forwarding` is branch-only candidate state until merged.
- `fix/subscription-rebind-confidence` is an extraction source, not a merge target and not current truth.
- Older `docs/plans/*` files may still say `status: active` but are historical planning context.
- `CLAUDE.md` and this file are repo-local CE adapters, not a second workflow system.

## Subagent Orchestration

Subagents are encouraged for bounded work because they protect the main agent’s context window.

Use subagents by default for:

- branch/worktree audits
- claim inventory and doc classification
- targeted code verification on `main`
- PR review slices
- git archaeology

Rules:

- subagents gather evidence; they do not declare final project truth
- the main agent makes the final state classification
- every subagent result should be framed as one of:
  - verified on `main`
  - verified off-main
  - inferred

Avoid giant undifferentiated swarms. Prefer one subagent per bounded question.

## PR Truth Pass

Every roadmap-affecting PR should complete this checklist after merge:

- [ ] merged code exists on `main`
- [ ] `docs/PROJECT-STATE-SNAPSHOT.md` still matches `main`
- [ ] `docs/PLAN.md` still matches `main`
- [ ] branch-only wording removed or explicitly retained as branch-scoped
- [ ] remaining-work lists revalidated

## Default Safe Behavior

If a statement conflicts with `main`, `main` wins.
