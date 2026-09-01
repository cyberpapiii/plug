---
status: complete
priority: p1
issue_id: "023"
tags: [code-review, reliability, architecture, issue-7]
dependencies: []
---

# Timeouts count as circuit breaker failures, causing cascading server lockout

## Problem Statement

When a tool call times out, `cb.on_failure()` is called, counting it toward the circuit breaker's failure threshold (default: 5). Calling a legitimately slow tool (like Slack `conversations_unreads`) 5 times trips the circuit breaker and locks out ALL tools on that server — including fast tools like `channels_list` that complete in <1s. The circuit breaker, designed to protect the system, amplifies failures instead.

**Likely contributor to issue #7 problem 3 (Workspace intermittent timeouts).**

## Findings

- **Source**: performance-oracle, architecture-strategist, security-sentinel (all flagged independently)
- **Location**:
  - Timeout arm: `plug-core/src/proxy/mod.rs:341-364` — `cb.on_failure()` at line 349
  - Circuit breaker threshold: `plug-core/src/circuit.rs:66` — `failure_threshold: 5`
  - Circuit breaker open duration: `plug-core/src/circuit.rs:68` — `open_duration: 30s`
- **Evidence**: The timeout `Err(_)` arm at proxy/mod.rs:349 calls `cb.on_failure()` identically to the actual error arm at line 325. No distinction between "server is down" and "this specific tool is slow."

## Proposed Solutions

### Option A: Remove cb.on_failure() from timeout arm (Recommended)
One-line change: delete `cb.on_failure()` at proxy/mod.rs:349. Timeouts indicate a slow tool, not a broken server. Only connection/protocol errors should trip the breaker.
- **Pros**: Simplest possible fix, surgically precise, prevents cascading lockout
- **Cons**: A truly hung server that times out won't trip the breaker (health checks still detect it)
- **Effort**: Small (1 line)
- **Risk**: Low

### Option B: Separate timeout threshold
Add a higher threshold for timeouts (e.g., 20) vs connection errors (5). Allow the circuit breaker to distinguish failure types.
- **Pros**: More nuanced, catches persistently slow servers
- **Cons**: Adds complexity to CircuitBreaker, more config surface
- **Effort**: Medium
- **Risk**: Low

## Acceptance Criteria

- [x] Tool call timeouts do not trip the circuit breaker
- [x] Fast tools remain callable even when slow tools on the same server timeout
- [x] Actual connection/protocol errors still trip the circuit breaker
- [x] Timeout path is covered by current proxy tests and code comments

## Work Log

### 2026-03-06 - Closed As Already Resolved

**By:** Codex

**Actions:**
- Verified in `plug-core/src/proxy/mod.rs` that the timeout arm no longer calls `cb.on_failure()`
- Confirmed the current timeout behavior with focused proxy tests

**Learnings:**
- Like `022`, this was a valid historical finding that became stale after the timeout/circuit-breaker fix landed.
