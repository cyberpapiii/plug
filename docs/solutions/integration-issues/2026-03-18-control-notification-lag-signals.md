---
title: Control-channel lag now emits downstream-visible warning signals
date: 2026-03-18
category: integration-issues
status: completed
---

# Control-channel lag now emits downstream-visible warning signals

## Problem

When the control notification channel lagged, `plug` only logged a local warning.
Downstream clients could miss `progress`, `cancelled`, or `list_changed` traffic
without receiving any protocol-visible hint that delivery had degraded.

## Solution

- introduced one shared structured logging payload for control-channel lag
- stdio downstreams now receive that warning through `notifications/message`
- HTTP downstreams receive the same warning through SSE fanout
- daemon IPC clients receive the warning as `LoggingNotification`

## Key decision

This tranche adds explicit visibility rather than trying to guarantee lossless
delivery for every control notification.

Why:

- a warning signal is a safe behavior change that preserves compatibility
- it closes the “silent divergence” problem immediately
- stronger queue/coalescing guarantees can build on top of this without needing
  to guess whether lag was happening in production
