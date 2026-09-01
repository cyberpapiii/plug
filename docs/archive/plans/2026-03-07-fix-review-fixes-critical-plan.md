---
title: "fix: review critical fixes"
type: fix
status: completed
date: 2026-03-07
origin: docs/brainstorms/2026-03-07-review-fixes-critical-brainstorm.md
---

# Review Critical Fixes

## Overview

Address the verified critical review findings that remained true on current `main` after the Phase
2 / Phase 3 rollout.

## Acceptance Criteria

- [x] Authenticated HTTP upstreams use the real bearer token value instead of `[REDACTED]`
- [x] Daemon IPC proxy supports resources/prompts parity with direct mode
- [x] Daemon IPC proxy capabilities are sourced from the daemon runtime rather than hardcoded
- [x] Active-call tracking is cleaned up safely across dropped/error paths
- [x] Resource URI collisions emit a warning instead of staying silent
- [x] Notification JSON conversion no longer panics on serialization failure
- [x] Workspace verification passes

## Deferred On Purpose

- `resources/subscribe`
- case-insensitive route lookup optimization
- any tool-call rate limiting
