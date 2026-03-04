---
status: pending
priority: p3
issue_id: "018"
tags: [code-review, performance]
dependencies: []
---

# Dirty flag set on events with no visual change

## Problem Statement

`ConfigReloaded` and `CircuitBreakerTripped` set `dirty = true` without updating any rendered state. Navigation keys set dirty even when selection doesn't change.

## Findings

- **Source**: performance-review
- **Locations**:
  - `app.rs:553-556` — ConfigReloaded/CircuitBreakerTripped
  - `app.rs:351-358` — navigation keys

## Acceptance Criteria

- [ ] Events only set dirty when they change visible state
- [ ] Navigation returns bool indicating whether selection changed
