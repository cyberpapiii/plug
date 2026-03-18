---
title: Legacy SSE fallback and endpoint timeout hardening
date: 2026-03-18
category: integration-issues
status: completed
---

# Legacy SSE fallback and endpoint timeout hardening

## Problem

The legacy SSE path had two brittle behaviors:

- fallback from Streamable HTTP to legacy SSE relied on broad string matching,
  which risked treating unrelated HTTP failures as legacy-SSE compatibility
- the SSE `endpoint` event had a hardcoded 5-second wait, even when the server
  startup timeout was configured much higher

## Solution

- legacy SSE fallback now keys on typed legacy-SSE hints plus one narrow
  compatibility signature for the known 405 initialize failure path
- the HTTP upstream connection now preserves the original error in the anyhow
  chain so fallback classification can inspect the real transport cause
- endpoint wait now uses the server’s configured startup timeout instead of an
  internal fixed 5-second deadline

## Key decision

The fallback kept a single explicit 405 compatibility signature instead of
removing all string-based fallback logic.

Why:

- the Streamable HTTP transport does not expose the 405 case as `LegacySseError`
  directly in the current stack
- preserving only the known 405 legacy signature keeps the working fallback path
  without reopening the broader 400/404 false-positive behavior

## Tests added

- generic HTTP status strings no longer trigger legacy SSE fallback
- endpoint waiting honors the provided timeout
- existing `http_upstream_falls_back_to_legacy_sse` integration coverage still
  passes with the narrower fallback behavior
