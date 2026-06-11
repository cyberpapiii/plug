---
title: "Restart the plug daemon in the user's login session, never from a detached/sandboxed context"
date: "2026-06-11"
category: integration-issues
module: plug daemon / oauth / keychain
problem_type: integration_issue
component: background_job
severity: high
symptoms:
  - "Daemon process is alive and connects upstreams but never binds plug.sock (no socket, no plug.pid, no 'daemon started' log)"
  - "Startup hangs after the non-OAuth upstreams connect; the OAuth servers (keychain-backed) never start"
  - "Killing the running daemon spawns many competing daemons that never converge"
  - "plug status reports 'Runtime unavailable' even though daemon processes are running"
root_cause: missing_workflow_step
resolution_type: workflow_improvement
tags:
  - daemon
  - keychain
  - oauth
  - ipc-socket
  - process-spawn
  - competing-daemons
  - operations
---

# Restart the plug daemon in the user's login session, never from a detached/sandboxed context

## Problem

Activating a freshly-installed `plug` binary by restarting the daemon from inside an agent/automation shell (or any detached, non-GUI-session context) took the whole multiplexer down. The daemon would start, connect most upstreams, then hang forever without binding its IPC socket — and aggressive retries cascaded into a multi-daemon storm.

## Symptoms

- A `plug serve --daemon` process is alive but `~/Library/Application Support/plug/plug.sock` never appears, `plug.pid` is never written, and the log never reaches "daemon started".
- The daemon log shows the non-OAuth upstreams starting (context7, exa, figma, slack, workspace, …) and then goes silent — the OAuth servers (notion, todoist, krisp) never start.
- `plug status` reports `Runtime unavailable` despite live daemon processes.
- Killing the daemon while `plug connect` clients are running spawns one new daemon **per connect client** (e.g. 7 → quickly 20+), none of which binds the socket.

## What Didn't Work

- **Killing the daemon (`kill <pid>`) while connect clients were live.** Each live `plug connect` auto-spawns a daemon when the socket disappears, so N clients respawn N daemons simultaneously. They race the single-instance pid-lock / socket bind and none converges.
- **Clearing `plug.sock` / `plug.pid` and retrying.** The race simply repeats — the connect clients re-spawn together again.
- **Killing orphaned upstream subprocesses (e.g. 15 stale `figma-console-mcp` instances squatting on ports 9223–9230).** Necessary cleanup, but it only unblocked the *figma* contention and exposed the real hang one layer down.
- **Starting a single daemon by hand from the agent shell** (`nohup plug serve --daemon &`, even with the sandbox disabled). A lone daemon with no race *still* hung — proving it was not a concurrency problem.

## Root cause

Two compounding issues, neither of which is a code regression:

1. **macOS Keychain reads block indefinitely without login-session GUI access.** Verbose tracing showed the hang was the final discovery step: `keyring: get password from entry MacCredential { service: "plug" }`. The OAuth upstreams' credentials live in the macOS Keychain. A process started from a detached/sandboxed context has no GUI session to satisfy the Keychain access prompt, so the credential read blocks forever — and because `run_daemon` starts the engine (all upstream connects, including the keychain-backed OAuth ones) **before** it binds the IPC socket, the whole daemon hangs before the socket ever appears. The original daemon worked precisely because launchd / the MCP host apps spawn it inside the user's login session, where the Keychain ACL is already approved.
2. **`plug connect` auto-spawns a daemon per client.** When the shared daemon dies, every live connect client independently spawns a replacement. Kill-with-live-clients therefore turns one daemon into many.

## Solution

**Start (or restart) the daemon in the user's login session, and never kill it out from under live connect clients.**

- To activate a newly-installed binary: run `cargo install --path plug --force`, then let the **MCP host apps** respawn their `plug connect` clients (they run in the login session and auto-spawn a daemon with Keychain access), **or** run `plug start` in the user's own Terminal. Do not `kill` the daemon from an automation/sandboxed shell.
- If a clean restart is needed, prefer restarting the host apps (Claude Desktop, the Claude Code session) over killing the daemon directly — that avoids the per-client respawn storm and guarantees login-session Keychain access.

## Why This Works

A login-session process inherits the approved Keychain ACL for `plug`, so the OAuth credential read returns immediately and `start_all` completes, letting `run_daemon` reach the socket bind. Bringing the daemon up through the host apps (or a single `plug start`) means exactly one daemon establishes the socket; the connect clients then attach to it instead of racing to spawn their own.

## Prevention

- **The merge is not the install.** Landing code on `main` does nothing to the running binary — `cargo install --path plug --force` replaces `~/.cargo/bin/plug`, and the services must restart to pick it up. Answer "is the latest installed?" with that, and stop.
- **Never restart the plug daemon from a non-login-session context** (agent shells, `nohup`, launchd jobs that aren't in the GUI session). Keychain-backed OAuth will hang the whole startup.
- **Never `kill` the daemon while `plug connect` clients are live** — it triggers a competing-daemon storm. Restart the host apps instead, or accept that the connects must re-converge.
- **Concurrent daemon starts also race OAuth `refresh_token` exchanges** (refresh tokens are single-use/rotating), which can invalidate stored tokens and force a re-auth. One more reason to keep daemon startup single-instance and session-scoped.
- Latent robustness gap worth a follow-up: a keychain-backed upstream that blocks should not hang daemon startup before the socket binds — bounding/deferring upstream connects (or binding the socket first) would make startup resilient to a hung OAuth read.
