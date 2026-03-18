---
title: Final P3 polish and runtime model cleanup
date: 2026-03-18
category: code-quality
status: completed
---

# Final P3 polish and runtime model cleanup

## Problem

After the correctness and operator-truth fixes landed, a few lower-priority
cleanup items still remained:

- daemon/runtime availability was still derived slightly differently across
  multiple views
- `reload.rs` still carried a more generic helper shape than the code needed
- the SSE/session broadcast path still used multiple follow-up vectors and the
  logging fanout path still bypassed the main serialization helper
- the workspace still used Tokio’s broad `full` feature set even though the
  codebase only exercised a narrower subset

## Solution

- introduced a small shared daemon-query availability model in `plug/src/runtime.rs`
  and reused it in the remaining operator surfaces
- simplified reload batching to use a direct helper over `ServerManager`
  rather than a one-off generic mini-framework
- reduced broadcast follow-up bookkeeping to one action list and routed logging
  fanout through the same SSE serialization helper as other notifications
- narrowed the workspace Tokio feature set from `full` to the specific features
  the repository actually uses

## Key decisions

- this pass stayed intentionally in `P3` territory: readability, maintainability,
  and low-risk runtime/compile improvements rather than behavior redesign
- the Tokio feature narrowing was only taken after a full workspace compile and
  test pass validated the smaller feature set
- operator-surface cleanup reused the same runtime truth vocabulary already
  established in the prior hardening pass rather than inventing a new status model

## Verification

- focused tests for reload, overview/status JSON, doctor env checks, and
  session broadcast behavior
- full workspace verification with `cargo test --workspace --quiet`
