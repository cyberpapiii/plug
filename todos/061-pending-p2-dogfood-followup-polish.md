---
status: pending
priority: p2
issue_id: "061"
tags: [ux, dogfood, polish, operator, auth, runtime]
dependencies: ["060"]
---

# Dogfood Follow-Up Polish

## Problem Statement

The broad operator/auth/runtime cleanup is on `main`, but the next meaningful improvements should
come from real usage rather than another speculative sweep. There will still be small rough edges,
copy issues, confusing states, or recovery gaps that only show up while using `plug` day to day.

## Goal

Keep one lightweight place to capture and execute real dogfood findings without reopening a large
architecture phase.

## Scope

Use this tracker for issues like:

- confusing wording in `status`, `clients`, `auth status`, or `doctor`
- unclear live vs linked vs fallback behavior
- recovery flows that still require too much guesswork
- auth/runtime edge cases that surface awkwardly in normal use
- small UI/data mismatches that don’t warrant a new broad program

## Task List

- [ ] Task 1: capture real usage findings as they occur
- [ ] Task 2: group findings into copy, UX, auth/recovery, or runtime buckets
- [ ] Task 3: execute small fixes in narrow, well-verified slices
- [ ] Verification: each landed fix has focused tests or live smoke evidence

## Intake Notes

Start with issues observed directly while using the current `main` build. Prefer concrete repros and
exact command output over general impressions.

## Work Log

### 2026-03-17 - Tracker created

**By:** Codex

**Actions:**
- Created a dedicated post-polish tracker so remaining work can be driven by real dogfooding.
- Explicitly scoped this as a narrow follow-up lane, not a new architecture program.

**Learnings:**
- The system is now clean enough that the highest-value remaining work is best discovered through
  actual daily use.
