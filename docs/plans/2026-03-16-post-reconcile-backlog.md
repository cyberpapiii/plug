# Post-Reconcile Backlog

Date: 2026-03-16

Purpose: capture the remaining work after reconciling the working branch into `main`.

This is not a blocker list for the current release bar. It is the practical next-work list after
the project baseline was stabilized and the branch/runtime reconciliation was completed.

## Current Situation

- `origin/main` now includes the previously working branch line.
- The reconciled line passed the full test suite before push.
- The current installed binary was intentionally left untouched and backed up separately.
- The project is still usable and stable, but there is known polish/follow-up work left.

## Known Open Items

### 1. HTTP session UX parity

Source:

- [`todos/056-pending-p2-http-session-ux-parity.md`](../../todos/056-pending-p2-http-session-ux-parity.md)

Why it matters:

- HTTP behavior works, but session ergonomics and user experience are not yet as polished as the
  better-covered paths.

### 2. Initialize protocol-version response simplification

Source:

- [`todos/055-pending-p3-initialize-protocol-version-response-simplification.md`](../../todos/055-pending-p3-initialize-protocol-version-response-simplification.md)

Why it matters:

- The current protocol-version behavior is working, but the code/path still carries cleanup debt.

### 3. Runtime reconfiguration beyond restart-based workflows

Source:

- [`docs/PLAN.md`](../PLAN.md)

Current status:

- optional future scope only

Why it matters:

- the current product is stable, but fully live runtime reconfiguration is still deferred

### 4. Post-reconcile cleanup and alignment

Remaining housekeeping:

- update current-truth docs to reflect the new `main` baseline commit after reconciliation
- clean up any leftover stale local branches/worktrees once no longer needed
- decide whether to replace the currently installed binary with a fresh install from `main`

## User-Observed Follow-Ups

Add issues here as they are noticed in real use. Keep each item concrete and reproducible.

Suggested format:

- short title
- exact command or workflow
- expected behavior
- actual behavior
- whether it is intermittent or consistent

### Placeholder entries

- remote connector rough edges noticed during week-of-use
- auth/reconnect friction that does not fully break flows but feels wrong
- HTTP/session behavior that works but feels less polished than stdio
- CLI/status/overview polish gaps that make troubleshooting harder

## Recommended Next Order

1. Refresh truth docs to the new `main` baseline commit.
2. Decide whether to switch the installed binary to a fresh build from `main`.
3. Triage the user-observed issues into bugs vs polish.
4. Resolve `todo 056`.
5. Resolve `todo 055`.
6. Reassess whether live runtime reconfiguration is still desirable after real usage feedback.
