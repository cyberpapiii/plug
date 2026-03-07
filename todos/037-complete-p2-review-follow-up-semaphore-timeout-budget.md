---
status: complete
priority: p2
issue_id: "037"
tags: [code-review, performance, proxy]
dependencies: []
---

# Align semaphore wait with server call-timeout budget

## Problem Statement

The initial semaphore wait fix used a hard-coded 30 second timeout, which could exceed a server’s configured `call_timeout_secs` and inflate tail latency under contention.

## Findings

- `plug-core/src/proxy/mod.rs` used a fixed semaphore wait budget
- Each upstream server already declares its own `call_timeout_secs`

## Recommended Action

Use the server’s `call_timeout_secs` as the semaphore acquisition budget when the upstream config is available.

## Acceptance Criteria

- [x] Semaphore wait is tied to the server’s configured call-timeout budget when possible
- [x] Overload errors still surface cleanly when capacity is exhausted

## Work Log

### 2026-03-06 - Completed During Review Follow-up

**By:** Codex

**Actions:**
- Derived semaphore wait timeout from upstream `call_timeout_secs`
- Kept a fallback constant only for test/no-upstream paths

**Learnings:**
- Queueing and execution budgets should use the same server-local timeout story unless there is a deliberate reason to separate them.
