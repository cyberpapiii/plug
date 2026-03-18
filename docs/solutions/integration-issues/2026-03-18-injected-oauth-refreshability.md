---
title: Injected OAuth credential refreshability follows configured client identity
date: 2026-03-18
category: integration-issues
status: completed
---

# Injected OAuth credential refreshability follows configured client identity

## Problem

`plug auth inject` previously stored all injected credentials with the synthetic
client id `injected`.

That had two bad effects:

- refresh could never work because the refresh path explicitly rejected
  `client_id == "injected"`
- the CLI claimed background refresh would work whenever a refresh token was
  present, even when the stored credential shape made refresh impossible

## Solution

- when the target server is configured for OAuth and has a concrete
  `oauth_client_id`, injected credentials are stored under that client id
- those injected credentials then participate in the normal refresh path
- when the server config does not provide a usable OAuth client identity,
  `plug` keeps the synthetic `injected` marker and tells the operator that
  auto-refresh is unavailable

## Key decision

Refreshability is derived from the configured server identity, not from whether
the injected token payload happened to include a refresh token.

Why:

- a refresh token alone is not enough; the refresh path still needs a valid
  OAuth client identity
- surfacing “refreshable: no” is more honest than promising refresh and failing
  later in the daemon
