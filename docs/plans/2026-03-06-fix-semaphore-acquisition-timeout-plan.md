---
title: "fix: semaphore acquisition timeout"
type: fix
status: active
date: 2026-03-06
origin: docs/brainstorms/2026-03-06-semaphore-acquisition-timeout-brainstorm.md
---

# Semaphore acquisition timeout

## Overview

Prevent indefinite blocking in `ToolRouter::call_tool_inner()` by bounding semaphore acquisition time and surfacing a clear overload error.

## Problem Statement / Motivation

A slow in-flight call on a server with `max_concurrent = 1` can block every subsequent request forever because the timeout only wraps the upstream call after the semaphore has already been acquired.

## Proposed Solution

- Add a semaphore acquisition timeout in `plug-core/src/proxy/mod.rs`
- Add a dedicated protocol error for the overload case in `plug-core/src/error.rs`
- Add focused tests for the new failure mode
- Close todo `024` once tests pass

## Acceptance Criteria

- [ ] Semaphore acquisition does not block indefinitely
- [ ] Client receives a clear overload error when the wait times out
- [ ] Existing tool-call timeout behavior remains unchanged after permit acquisition
- [ ] Focused tests cover the new overload path

## Sources & References

- **Origin brainstorm:** docs/brainstorms/2026-03-06-semaphore-acquisition-timeout-brainstorm.md
- `plug-core/src/proxy/mod.rs`
- `plug-core/src/error.rs`
- `todos/024-pending-p2-semaphore-acquisition-no-timeout.md`
