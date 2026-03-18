---
title: Auth status now surfaces keyring and token-file backing store drift
date: 2026-03-18
category: integration-issues
status: completed
---

# Auth status now surfaces keyring and token-file backing store drift

## Problem

Credential persistence drift had become visible in logs, but not to operators:

- the OAuth store could warn when the keyring write succeeded but the file
  mirror write failed
- `plug auth status` and daemon `AuthStatus` still only exposed
  authenticated/health/expiry state
- that meant a broken mirror or a disagreement between keyring and token file
  was easy to miss until a later restart or reconnect behaved unexpectedly

## Solution

- the OAuth store now reports backing-store warnings when keyring and token file
  are missing on one side or disagree on persisted token identity/timestamp
- daemon `AuthStatus` includes a per-server `warnings` field
- CLI `plug auth status` now prints those warnings in text mode and includes
  them in the stable JSON envelope

## Key decision

Warnings are informational signals rather than hard auth failures.

Why:

- drift does not necessarily mean credentials are unusable right now
- the freshest credential can still be loaded and used safely
- treating drift as a warning preserves truthful operator visibility without
  incorrectly escalating healthy-but-inconsistent persistence into auth failure

## Tests added

- store-level warning detection for a file-only persisted credential case
- daemon auth-status tests now assert warning plumbing matches the store view
- auth-status JSON tests verify the warnings field is preserved in the output
