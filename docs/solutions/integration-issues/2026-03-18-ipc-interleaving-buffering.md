---
title: IPC interleaving buffering during daemon registration and roots updates
date: 2026-03-18
category: integration-issues
status: completed
---

# IPC interleaving buffering during daemon registration and roots updates

## Problem

Two IPC paths still assumed a response-only stream even though the daemon can
push notifications on registered connections:

- daemon session establishment (`Register` → `Capabilities`)
- roots refresh (`UpdateRoots`)

That meant a valid logging or control notification could be mistaken for the
expected response, desynchronizing the connection or failing registration.

## Solution

- session establishment now reads until the expected response while buffering
  interleaved notifications
- buffered notifications are carried into the proxy session and flushed after
  the downstream peer is available
- roots refresh now forwards interleaved control notifications and logging
  instead of treating them as protocol noise

## Key decision

Buffered notifications are replayed only after initialize finishes and the
downstream peer is set.

Why:

- registration happens before the proxy has a peer to forward to
- dropping those notifications would preserve the old bug
- trying to process them before initialize would introduce a second protocol
  ordering problem on the downstream side

## Result

Daemon-backed stdio sessions now treat registration/capabilities and roots
updates like the rest of the IPC proxy path: notifications are part of the
stream, not protocol violations.
