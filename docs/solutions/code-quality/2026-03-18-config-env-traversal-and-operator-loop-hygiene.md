---
title: Centralized config env traversal and refreshed operator loop state
date: 2026-03-18
category: code-quality
status: completed
---

# Centralized config env traversal and refreshed operator loop state

## Problem

There were two small but persistent hygiene issues:

- config env expansion and raw env-reference extraction each manually walked the
  same set of fields, which made config-surface growth drift-prone
- interactive `plug servers` and `plug tools` views captured daemon
  availability once before entering their action loops, so later loop
  iterations could render stale runtime state after config or daemon changes

There was also one remaining auth-status regression risk:

- the zero-server JSON case needed an explicit regression to keep the stable
  auth-status envelope from drifting again

## Solution

- config env traversal is now centralized through shared helper functions:
  one visitor for read-only extraction and one transformer for expansion
- `plug servers` and `plug tools` now re-check daemon availability on each loop
  iteration instead of caching the result before any interactive mutations
- the auth-status empty JSON case now has a regression test that locks in the
  stable envelope shape

## Key decisions

- this tranche stays focused on hygiene and correctness, not broad structural
  churn
- the Cargo feature review did not produce a clearly safe narrowing change in
  this pass, so no dependency-feature edits were made without stronger
  evidence
- the integration-test surface already had substantial helper coverage at the
  top of the file, so this pass prioritized removing active drift risks first

## Tests added

- auth-status empty JSON envelope regression coverage
- existing tool empty-state tests still pass with loop-time daemon refresh
- full workspace tests pass after the hygiene refactor
