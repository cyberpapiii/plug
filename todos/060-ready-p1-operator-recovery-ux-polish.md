---
status: ready
priority: p1
issue_id: "060"
tags: [ux, operator, recovery, clients, status, auth, doctor, clarity]
dependencies: ["057", "058", "059"]
---

# Operator Recovery UX Polish

## Problem Statement

The runtime/auth/topology truth is now much better, but the operator CLI still makes users work too
hard to understand what path is active and what to do next. The remaining issues are not protocol
or runtime correctness problems; they are clarity and recovery-flow problems.

## Goal

Make `plug clients`, `plug status`, `plug auth status`, and `plug doctor` feel like one coherent
operator experience:

- short, explicit state labels
- obvious linked vs live distinctions
- deterministic next actions
- less table noise and less cross-screen mental joining

## References

- [docs/plans/2026-03-17-operator-recovery-ux-polish-plan.md](../docs/plans/2026-03-17-operator-recovery-ux-polish-plan.md)
- [docs/PLAN.md](../docs/PLAN.md)

## Task List

- [x] Task 1: polish `plug clients` transport/live state presentation
- [x] Task 2: polish `plug status` runtime and recovery wording
- [x] Task 3: polish `plug auth status` source/recovery wording
- [x] Task 4: polish `plug doctor` grouping and next-step guidance
- [ ] Task 5: run final consistency/copy pass across all four surfaces
- [ ] Verification: focused tests pass
- [ ] Verification: live smoke outputs look coherent

## Work Log

### 2026-03-17 - Program created

**By:** Codex

**Actions:**
- Split the remaining post-architecture operator polish work into its own tracked phase.
- Defined the next surfaces to tighten: `clients`, `status`, `auth status`, and `doctor`.
- Framed the work around clarity, simplicity, and deterministic recovery guidance.

**Learnings:**
- The remaining product work is mostly about reducing operator confusion in real usage.
- The right bar is no longer “expose more truth”; it is “expose the truth clearly enough that the
  user can act without cross-referencing several commands.”

### 2026-03-17 - Operator UX polish tranche 1

**By:** Codex

**Actions:**
- Simplified `plug clients` configured inventory so live transport is visible directly in the
  configured row (`live via http`, `live via daemon_proxy`) instead of only in the separate session
  table.
- Restructured the configured-clients presentation so detailed link topology moves onto a secondary
  line rather than stretching the main table.
- Tightened `plug status` summary language around linked clients and linked topology.
- Collapsed `Inventory Scope` + `Inventory Availability` into one `Live Inventory` summary value in
  `clients` and `status`.
- Added a stronger fallback warning to `plug auth status` when live daemon auth state is
  unavailable.
- Added a `doctor` `Next` section that deduplicates and summarizes actionable recovery steps after
  the raw checks.
- Added regression coverage for the new summary helpers and doctor action grouping.

**Verification:**
- `cargo test -p plug views::clients -- --nocapture`
- `cargo test -p plug views::overview -- --nocapture`
- `cargo test -p plug commands::auth::tests -- --nocapture`
- `cargo test -p plug commands::misc::tests -- --nocapture`
- live smoke:
  - `cargo run --quiet --bin plug -- clients`
  - `cargo run --quiet --bin plug -- status`
  - `cargo run --quiet --bin plug -- auth status`
  - `cargo run --quiet --bin plug -- doctor`

**Learnings:**
- The biggest remaining UX wins are about reducing joins between sections, not exposing more raw
  runtime data.
- A compact summary label plus one explanatory line works better than two adjacent labels
  (`Inventory Scope` + `Inventory Availability`) for the same concept.
- `doctor` becomes much easier to act on once the command ends with deduplicated operator steps.
