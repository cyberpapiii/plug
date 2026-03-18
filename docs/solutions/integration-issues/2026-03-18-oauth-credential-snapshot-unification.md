---
title: OAuth credential reads now use one freshest-persisted snapshot path
date: 2026-03-18
category: integration-issues
status: completed
---

# OAuth credential reads now use one freshest-persisted snapshot path

## Problem

The OAuth store still had two different read behaviors:

- `current_or_stored_access_token()` used freshest-persisted credential
  selection across cache, file mirror, and keyring
- `CredentialStore::load()` still used the older `cache -> file -> keyring`
  ordering
- auth-status rendering also re-read the store multiple times for credentials,
  expiry, and warnings

That left one correctness bug and one repeated-cost problem:

- a stale token file could mask a fresher keyring credential for `load()`-based
  callers
- auth-status collection did more backing-store work per OAuth server than it
  needed to

## Solution

- the store now exposes one internal credential snapshot path that selects the
  freshest available credentials and returns:
  - the selected credential bundle
  - selected source
  - expiry timing
  - backing-store warnings
- `CredentialStore::load()` now uses that same snapshot path
- daemon auth status and CLI fallback auth status now read from the snapshot
  instead of doing separate `load()` and warning probes
- the empty JSON case for `plug auth status` now returns the full stable
  envelope even when there are no OAuth-configured servers

## Key decision

The snapshot path stays inside the store rather than adding a separate
cross-cutting auth-status builder.

Why:

- source selection, expiry timing, and drift warnings are all store concerns
- keeping them together avoids another layer of repeated file/keyring reads
- it preserves current operator semantics while removing duplicated read logic

## Tests added

- fresher persisted credentials still replace stale cached credentials
- daemon auth-status fallback still reports degraded runtime state correctly
- auth-status JSON keeps its stable envelope and warning fields
- full workspace tests pass with the unified snapshot path
