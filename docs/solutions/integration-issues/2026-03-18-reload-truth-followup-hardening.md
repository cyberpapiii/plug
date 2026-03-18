---
title: Reload serialization and operator truth follow-up hardening
date: 2026-03-18
category: integration-issues
status: completed
---

# Reload serialization and operator truth follow-up hardening

## Problem

The post-performance review found a second round of issues after the batching
and fanout work landed:

- concurrent reload startup could still lose successfully started servers
- reload requests from watcher, IPC, and signals were not single-flight
- targeted HTTP reverse delivery still degraded into timeout-backed waiting once
  send-time delivery failed
- operator surfaces still blurred “daemon unreachable” and “daemon reachable
  but IPC/runtime inspection failed”
- auth injection and auth-status rendering still had live-runtime drift and
  duplicated store IO
- doctor env inspection still drifted from the centralized config env traversal

## Solution

- server-map mutation is now guarded so concurrent upstream registration cannot
  overwrite sibling inserts
- engine reloads now run behind one async reload gate
- targeted reverse requests now use live-delivery outcome feedback instead of
  blindly waiting after send-time failure
- `status`, `servers`, `tools`, `clients`, and `doctor` now distinguish runtime
  unavailability from IPC/runtime inspection failure more explicitly
- `plug auth inject` refreshes the live daemon when possible, and
  `plug auth status` avoids redundant credential-store reads when live daemon
  state already answers the question
- doctor env checks now reuse the centralized config env traversal helpers, and
  those helpers cover a broader set of env-expandable config fields

## Key decisions

- the reload fix was applied at the shared mutation seam, not just inside reload,
  so future concurrent start paths inherit the safety improvement
- targeted reverse delivery keeps queued notification behavior for general
  fanout, but live reverse-request delivery now treats send-time failure as a
  hard failure instead of an eventual timeout
- operator JSON now prefers explicit availability/source fields over synthetic
  health guesses when runtime truth is missing

## Verification

- added a concurrency regression for concurrent upstream insertion
- full workspace tests pass after the follow-up hardening
