---
status: pending
priority: p2
issue_id: "027"
tags: [code-review, reliability, architecture, issue-7]
dependencies: ["023"]
---

# Health checks and circuit breaker amplify each other on slow servers

## Problem Statement

Health checks use a hardcoded 10s timeout (health.rs:99) and drive servers through Healthy → Degraded → Failed. Once Failed, tools are excluded from `get_tools()`. Combined with the circuit breaker (which also blocks calls), a transiently slow server faces a double penalty. Recovery requires 2 consecutive health check successes PLUS circuit breaker reset — but during recovery, the circuit breaker may independently block the health probes.

## Findings

- **Source**: security-sentinel, code-simplicity-reviewer
- **Location**:
  - Health check timeout: `plug-core/src/health.rs:99-108` (hardcoded 10s)
  - Health exclusion: `plug-core/src/server/mod.rs:284` (`health != Failed`)
  - Health state machine: `plug-core/src/types.rs:112-173` (HealthState)
- **Evidence**: Two independent failure-tracking systems (HealthState + CircuitBreaker) with overlapping concerns amplify each other on transiently slow servers.

## Proposed Solutions

### Option A: Remove HealthState, keep circuit breaker only (Recommended)
The circuit breaker already tracks failure state and provides Closed/Open/HalfOpen transitions. HealthState is redundant. Remove `health` DashMap from ServerManager, remove HealthState, let circuit breaker be the single source of truth.
- **Pros**: Eliminates dual-tracking, simplifies recovery, ~60 LOC reduction
- **Cons**: Loses the gradual Degraded state (though circuit breaker's HalfOpen serves similar purpose)
- **Effort**: Medium

### Option B: Decouple health checks from circuit breaker
Make health check failures not feed into circuit breaker, and vice versa.
- **Pros**: Both systems work independently, no amplification
- **Cons**: Still two systems tracking the same concept
- **Effort**: Small

## Acceptance Criteria

- [ ] A single failure-tracking mechanism determines server availability
- [ ] Transiently slow servers recover without double penalty
- [ ] Server availability state is consistent across all code paths
