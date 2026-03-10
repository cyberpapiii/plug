---
title: Claude remote HTTP connector stability
date: 2026-03-10
category: integration-issues
components:
  - plug-core/src/http/server.rs
  - plug-core/src/config/mod.rs
  - plug-core/src/proxy/mod.rs
  - plug-core/src/server/mod.rs
  - plug/src/ipc_proxy.rs
  - plug/src/runtime.rs
problem_type: remote-mcp-connector-instability
summary: Fixed a cluster of Claude remote-connector failures by debouncing downstream list-changed notifications, adding explicit HTTP origin allowlisting for Claude-hosted connectors, aligning stdio protocol advertisement to 2025-11-25, and documenting that the remote connector must target the full `/mcp` endpoint rather than the tunnel root.
related:
  - docs/bug-reports/claude-remote-mcp-no-tools-when-initialize-body-advertises-2025-06-18.md
  - docs/bug-reports/pagination-cursor-forwarding-and-remote-client-blanking.md
  - docs/solutions/integration-issues/downstream-https-serving-20260307.md
  - docs/solutions/integration-issues/phase2c-resources-prompts-pagination-20260307.md
  - docs/solutions/integration-issues/review-fixes-critical-http-auth-ipc-parity-20260307.md
---

# Claude Remote HTTP Connector Stability

## Problem

Claude Desktop and Claude Mobile were intermittently unusable against `plug serve` over a Cloudflare tunnel.

Observed symptoms:

- the custom connector would sometimes show tools briefly, then lose them
- the connector sometimes reported a generic auth/server URL error
- Desktop and Mobile could both amplify the failure, but a single Desktop connection could still reproduce it
- local stdio clients were also advertising an older MCP protocol version on initialize

The most confusing part was that these failures overlapped:

1. real server-side MCP issues
2. connector misconfiguration at the HTTP URL layer
3. Claude Desktop UI behavior that did not always surface the real failing connector in `mcp.log`

## Root causes

### 1. HTTP list-changed startup storms

Upstreams that advertise `tools/list_changed` can emit notifications while they are still settling after startup. `plug` would refresh and immediately republish downstream `ToolListChanged` on every refresh cycle, which was enough to churn Claude’s connector state.

The code was single-flight, but not debounced, so bursty upstream notifications still produced bursty downstream notifications.

### 2. HTTP origin gate rejected Claude-hosted connectors

`plug`’s HTTP middleware treated loopback-only serving as “no auth required, localhost origins only.” That is correct for direct local browser traffic, but wrong for a Claude-hosted custom connector arriving through Cloudflare with an `Origin` like:

- `https://claude.ai`
- `https://www.claude.ai`

Those requests were getting rejected with `403 forbidden`, which Claude surfaced as a generic “check your server URL / auth” message.

### 3. Connector URL needed the actual MCP endpoint

The Claude custom connector must point at the full MCP endpoint:

`https://<public-host>/mcp`

Using only the tunnel root:

`https://<public-host>`

caused Claude to behave like it had entered an auth/bootstrap path rather than talking to the MCP endpoint directly.

### 4. Stdio initialize path still advertised 2025-06-18

The daemon-backed stdio path used `InitializeResult::new(...)`, which inherits rmcp’s default protocol version (`2025-06-18`). HTTP had already been patched to advertise `2025-11-25`, so stdio and HTTP were inconsistent.

## Solution

### 1. Debounce downstream list-changed refreshes

In [plug-core/src/proxy/mod.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/proxy/mod.rs):

- added a short debounce window before `refresh_tools()`
- kept the existing pending flags and single-flight loop
- preserved eventual consistency while collapsing bursts into one downstream notification per wave

This reduced startup churn without changing the overall refresh model.

### 2. Add explicit `http.allowed_origins`

In [plug-core/src/config/mod.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/config/mod.rs) and [plug-core/src/http/server.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/http/server.rs):

- added `http.allowed_origins`
- checked exact `Origin` matches before the localhost-only fallback
- kept authenticated requests bypassing origin checks as before

This allows remote Claude connectors without weakening the default loopback-origin policy.

For the working local config, the allowed origins were:

- `https://claude.ai`
- `https://www.claude.ai`

### 3. Require the connector to use `/mcp`

The final working Cloudflare connector URL was:

`https://<public-host>.trycloudflare.com/mcp`

not just the root host.

This turned out to be essential. Once the URL included `/mcp`, the connector established cleanly.

### 4. Align stdio initialize protocol advertisement

In [plug/src/ipc_proxy.rs](/Users/robdezendorf/Documents/GitHub/plug/plug/src/ipc_proxy.rs):

- explicitly set `2025-11-25` in the daemon-backed initialize response
- added a regression proving the downstream stdio peer sees `2025-11-25`

## Verification

Verified locally with both tests and live probes.

### Automated

- full `cargo test` passed
- added router-level debounce regression
- added end-to-end stdio regression proving rapid upstream `tools/list_changed` notifications collapse before downstream delivery
- added HTTP regression proving an allowlisted external origin is accepted
- added stdio regression proving daemon-backed initialize advertises `2025-11-25`

### Live probes

Verified all of the following:

- local HTTP `POST /mcp initialize` returned `200 OK`
- local HTTP initialize with `Origin: https://claude.ai` returned `200 OK` after the allowlist change
- public Cloudflare-tunneled `POST /mcp initialize` returned `200 OK`
- `plug connect` initialize returned `protocolVersion: 2025-11-25`

### Real client outcome

After updating the connector URL to the full `/mcp` endpoint and using the fresh tunnel URL:

- Claude Desktop HTTP connector worked
- Claude Mobile HTTP connector worked
- Claude Code stdio still worked

## Prevention

- Keep one explicit protocol-version constant for downstream advertised surfaces. Do not rely on rmcp defaults for `InitializeResult::new(...)`.
- For Claude-hosted HTTP connectors, document the required allowlisted origins and the requirement to use the full `/mcp` endpoint.
- If remote connectors fail but `plug` logs show no new downstream HTTP session, suspect the connector URL first.
- When debugging Desktop connector failures, do not trust `mcp.log` alone. Claude may only show built-in connector churn there while the custom connector never reaches the server.
- Debounce downstream `listChanged` fan-out for bursty upstream startup waves so client instability does not mask deeper issues.

## Follow-up gap

The terminal/menu UX still appears incomplete for remote HTTP sessions. During this incident, active Claude Desktop HTTP usage was not clearly surfaced in the plug client/menu system, and the UX did not make it obvious which live session or transport a user was inspecting.

That gap should be treated as separate follow-up work:

- confirm whether HTTP sessions are absent from the current menu/session inventory or merely not labeled clearly
- make transport type explicit in session views
- ensure operators can distinguish Desktop/Mobile HTTP sessions from local stdio clients
- review whether any newer HTTP features landed without corresponding UX parity

## Smoke test recipe

When this regresses, verify in this order:

1. `curl` the public URL with `POST /mcp initialize`
2. include `Origin: https://claude.ai`
3. confirm `200 OK` and `protocolVersion: 2025-11-25`
4. verify the connector URL includes `/mcp`
5. verify `serve-stderr.log` shows a new `HTTP client connected` entry during the real client attempt

If Claude shows a connector error but no new `HTTP client connected` entry appears in `plug` logs, the request is not reaching `plug` and the problem is almost certainly in the connector URL or client-side connector state.
