# Workflow Operating Model

This repo uses **Compound Engineering (CE)** as the workflow operating system.

This document explains how to use CE safely in this repo without confusing `main`, PR branches,
worktrees, plans, and historical docs.

## Start Here

For current project state:

1. `docs/PROJECT-STATE-SNAPSHOT.md`
2. `docs/PLAN.md`
3. `main` code if the answer materially matters

Do not start with `docs/plans/*`, branch summaries, or historical research docs when the question
is “what is true now?”

## What Each Doc Family Means

- `docs/PROJECT-STATE-SNAPSHOT.md`
  Canonical current-state snapshot on `main`
- `docs/PLAN.md`
  High-level remaining work on `main`
- `docs/TRUTH-RULES.md`
  Repo-local truth and drift-prevention rules
- `docs/audit/*.md`
  Evidence and reconciliation backing docs
- `docs/plans/*.md`
  Intended work / implementation planning
- `docs/solutions/*.md`, `docs/research/*.md`
  Historical compound knowledge and archaeology
- `todos/*.md`
  Tracked issues and follow-up items

## Main vs Branches vs Worktrees

- `main` is the only source of “done now”
- branches/worktrees are candidate future state only
- a feature can exist on a branch and still be `missing` on `main`
- merged PR summaries are not evidence unless the merged code is present on `main`

## How To Use CE Here

Use normal CE flow:

- plan
- work
- review
- compound

Repo-local safety rules:

- always answer progress questions from the snapshot first
- keep branch-scoped plans explicitly branch-scoped
- do a post-merge truth pass for roadmap-affecting PRs

## Subagents

Subagents are encouraged for bounded work:

- branch/worktree audits
- claim inventory
- PR review slices
- git archaeology
- code verification on `main`

The main agent should make the final truth decision.
