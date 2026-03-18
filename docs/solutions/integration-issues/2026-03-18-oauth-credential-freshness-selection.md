---
title: OAuth credential selection now compares cache, file mirror, and keyring consistently
date: 2026-03-18
category: integration-issues
status: completed
---

# OAuth credential selection now compares cache, file mirror, and keyring consistently

## Problem

Credential freshness logic still had a blind spot:

- the store knew how to prefer a newer mirrored token file over a stale cache
- but it did not make the same freshness comparison across all three sources:
  cache, file mirror, and keyring
- a successful keyring save combined with a failed file-mirror save also stayed
  silent, which made persistence drift harder to notice

## Solution

- credential selection now compares cache, file, and keyring together instead
  of treating keyring as a last-resort fallback only
- newer persisted credentials rehydrate the in-memory cache regardless of
  whether they came from the file mirror or the keyring
- tied timestamps with different token identities now resolve deterministically,
  preferring the stronger persisted source rather than whichever branch happened
  to run last
- file-mirror save failures now emit an explicit warning when keyring save
  succeeded, so persistence drift is no longer silent

## Key decision

Timestamp comparison remains the primary freshness signal, with source priority
only used as a deterministic tie-breaker.

Why:

- `token_received_at` is the best cross-process freshness hint already present
  in the stored credential format
- tie-breaking by source avoids unstable behavior when two stores have the same
  timestamp but different token identities
- this keeps the change local to store selection logic rather than requiring a
  new persisted version vector or journal format

## Tests added

- fresher credentials win when timestamps differ
- keyring wins deterministic tie-breaking against the file mirror when
  timestamps match but token identities differ
- the existing reconnect-time token lookup test still passes when newer
  persisted credentials replace stale cached credentials
