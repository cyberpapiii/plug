---
title: "Roadmap Audit"
date: 2026-03-08
status: audited
audited_against:
  - docs/PLAN.md
  - docs/plans/2026-03-07-feat-mcp-spec-compliance-roadmap-plan.md
  - docs/plans/2026-03-07-feat-roadmap-tail-closeout-plan.md
  - todos/
---

# Roadmap Audit

This document is a factual audit of the currently tracked roadmap claims against the codebase on
`main` as of 2026-03-08.

Method:

- verify claims against code, not plan intent
- treat a feature as `done` only when the implementation exists in the live codepath
- use `partial` for intentionally bounded behavior, under-proven pass-through, or transport gaps
- use `missing` for code that is not implemented

## Summary

Using the actively tracked roadmap items, not the older speculative phase plans.

As of PR #35 merge (`0389b22`, 2026-03-09), Stream A protocol correctness, roots forwarding,
elicitation + sampling reverse-request forwarding, and legacy SSE upstream transport are complete on
`main`.

The remaining partial areas on `main` are transport-bounded or under-proven:

- reconnect-based daemon continuity proof is narrower than full cross-transport persistence

## Checklist

| Item | Status | Evidence / Gap |
|---|---|---|
| Downstream HTTP bearer auth | done | Non-loopback token generation and enforcement exist in [plug/src/runtime.rs](/Users/robdezendorf/Documents/GitHub/plug/plug/src/runtime.rs#L325), [plug-core/src/auth.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/auth.rs#L23), and [plug-core/src/http/server.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/http/server.rs#L215). |
| Logging notification forwarding | done | Upstream logging enters at [plug-core/src/server/mod.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/server/mod.rs#L112), routes through the dedicated logging channel in [plug-core/src/proxy/mod.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/proxy/mod.rs#L266), and fans out via stdio, HTTP, and IPC in [plug-core/src/proxy/mod.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/proxy/mod.rs#L2360), [plug-core/src/http/server.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/http/server.rs#L131), and [plug/src/daemon.rs](/Users/robdezendorf/Documents/GitHub/plug/plug/src/daemon.rs#L545). |
| Structured output pass-through: `outputSchema` | done | `strip_optional_fields()` no longer strips schemas in [plug-core/src/proxy/mod.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/proxy/mod.rs#L1967), with explicit test coverage at [plug-core/src/proxy/mod.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/proxy/mod.rs#L2656). |
| Structured output pass-through: `structuredContent` | done | Tool results are returned unchanged in [plug-core/src/proxy/mod.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/proxy/mod.rs#L1619), with dedicated stdio and HTTP end-to-end tests in [plug-core/tests/integration_tests.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/tests/integration_tests.rs). |
| Structured output pass-through: `resource_link` | done | `RawContent::ResourceLink` is preserved by the same pass-through path in [plug-core/src/proxy/mod.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/proxy/mod.rs#L1619), with dedicated stdio and HTTP end-to-end tests in [plug-core/tests/integration_tests.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/tests/integration_tests.rs). |
| Completion forwarding across stdio | done | The router forwards completion requests in [plug-core/src/proxy/mod.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/proxy/mod.rs#L1333) and exposes the handler at [plug-core/src/proxy/mod.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/proxy/mod.rs#L2542). |
| Completion forwarding across daemon IPC | done | IPC forwards `completion/complete` in [plug/src/ipc_proxy.rs](/Users/robdezendorf/Documents/GitHub/plug/plug/src/ipc_proxy.rs#L612), and the daemon dispatches it in [plug/src/daemon.rs](/Users/robdezendorf/Documents/GitHub/plug/plug/src/daemon.rs#L1081). |
| Completion forwarding across HTTP | done | PR #31 adds `CompleteRequest` handler in `plug-core/src/http/server.rs`. Routes through `state.router.complete_request()`. |
| Resources forwarding (`resources/list`, `resources/read`) | done | Merged catalogs and read routing exist in [plug-core/src/proxy/mod.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/proxy/mod.rs#L1015), [plug-core/src/proxy/mod.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/proxy/mod.rs#L1215), and [plug-core/src/proxy/mod.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/proxy/mod.rs#L1265), with stdio/HTTP/IPC handlers wired. |
| Resource templates forwarding | done | Template merging and pagination are implemented in [plug-core/src/proxy/mod.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/proxy/mod.rs#L1048) and [plug-core/src/proxy/mod.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/proxy/mod.rs#L1234), with stdio/HTTP/IPC handlers wired. |
| Prompts forwarding (`prompts/list`, `prompts/get`) | done | Prompt catalog merge and upstream routing exist in [plug-core/src/proxy/mod.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/proxy/mod.rs#L1066), [plug-core/src/proxy/mod.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/proxy/mod.rs#L1253), and [plug-core/src/proxy/mod.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/proxy/mod.rs#L1294). |
| Resource subscribe/unsubscribe for direct stdio and HTTP | done | Subscription bookkeeping and upstream fan-out are implemented in [plug-core/src/proxy/mod.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/proxy/mod.rs#L565), [plug-core/src/proxy/mod.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/proxy/mod.rs#L636), and [plug-core/src/http/server.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/http/server.rs#L605). |
| Resource subscription rollback on upstream failure | done | Failed first-subscribe rolls back local state in [plug-core/src/proxy/mod.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/proxy/mod.rs#L601). |
| Resource subscription cleanup on disconnect | done | Disconnect cleanup is implemented in [plug-core/src/proxy/mod.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/proxy/mod.rs#L678). |
| Resource subscription state after route refresh | done | PR #31 adds subscription pruning (stale URIs removed + unsubscribed upstream) and rebind (moved URIs resubscribed to new owner) in `refresh_tools()`. Dual-subscription prevented on rebind failure. Todo 039 closed. |
| Targeted `notifications/resources/updated` delivery | done | Upstream notification handling exists in [plug-core/src/server/mod.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/server/mod.rs#L99), routing in [plug-core/src/proxy/mod.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/proxy/mod.rs#L724), and stdio/HTTP fan-out in [plug-core/src/proxy/mod.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/proxy/mod.rs#L2334) and [plug-core/src/http/server.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/http/server.rs#L101). |
| Truthful resource subscribe capability synthesis | done | Capability synthesis is in [plug-core/src/proxy/mod.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/proxy/mod.rs#L1156), and daemon IPC masks subscriptions in [plug/src/daemon.rs](/Users/robdezendorf/Documents/GitHub/plug/plug/src/daemon.rs#L901). |
| Resource subscribe support over daemon IPC | partial | This is intentionally unsupported, not implemented. The daemon returns `UNSUPPORTED_METHOD` in [plug/src/daemon.rs](/Users/robdezendorf/Documents/GitHub/plug/plug/src/daemon.rs#L1205). The behavior is honest, but it is still a capability gap. |
| `resources/list_changed` forwarding | done | PR #31 adds handler + coalesced refresh; fan-out via stdio, HTTP, and daemon IPC (PR #38). |
| `prompts/list_changed` forwarding | done | PR #31 adds handler + coalesced refresh; fan-out via stdio, HTTP, and daemon IPC (PR #38). |
| Progress routing | done | End-to-end routing across stdio, HTTP, and daemon IPC. PR #38 adds IPC push frames with session-targeted delivery. |
| Cancellation routing | done | End-to-end routing across stdio, HTTP, and daemon IPC. PR #38 adds IPC push frames with session-targeted delivery. |
| `tools/list_changed` forwarding | done | Fan-out via stdio, HTTP, and daemon IPC. PR #38 adds IPC push frames. |
| `plug connect` stdio downstream surface | done | CLI dispatch and runtime behavior exist in [plug/src/main.rs](/Users/robdezendorf/Documents/GitHub/plug/plug/src/main.rs#L146) and [plug/src/runtime.rs](/Users/robdezendorf/Documents/GitHub/plug/plug/src/runtime.rs#L231). |
| `plug serve` Streamable HTTP downstream surface | done | CLI dispatch and HTTP serving exist in [plug/src/main.rs](/Users/robdezendorf/Documents/GitHub/plug/plug/src/main.rs#L149) and [plug/src/runtime.rs](/Users/robdezendorf/Documents/GitHub/plug/plug/src/runtime.rs#L311). |
| Optional HTTPS serving | done | TLS config and runtime binding exist in [plug-core/src/config/mod.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/config/mod.rs#L73) and [plug/src/runtime.rs](/Users/robdezendorf/Documents/GitHub/plug/plug/src/runtime.rs#L360). |
| Daemon-backed local sharing | done | Daemon mode, client registration, and shared routing exist in [plug/src/runtime.rs](/Users/robdezendorf/Documents/GitHub/plug/plug/src/runtime.rs#L153), [plug/src/daemon.rs](/Users/robdezendorf/Documents/GitHub/plug/plug/src/daemon.rs#L715), and [plug/src/daemon.rs](/Users/robdezendorf/Documents/GitHub/plug/plug/src/daemon.rs#L957). |
| Reconnecting IPC proxy sessions | done | Reconnect logic exists in [plug/src/ipc_proxy.rs](/Users/robdezendorf/Documents/GitHub/plug/plug/src/ipc_proxy.rs#L80) and [plug/src/ipc_proxy.rs](/Users/robdezendorf/Documents/GitHub/plug/plug/src/ipc_proxy.rs#L166), with restart recovery tests in the same file. |
| Daemon continuity recovery | partial | The code proves reconnect-based recovery for daemon-backed stdio clients, but not broad persisted continuity across all downstream session types. See [plug/src/ipc_proxy.rs](/Users/robdezendorf/Documents/GitHub/plug/plug/src/ipc_proxy.rs#L753) and [plug/src/daemon.rs](/Users/robdezendorf/Documents/GitHub/plug/plug/src/daemon.rs#L528). |
| Session-store abstraction / stateless prep | done | The abstraction seam is present in [plug-core/src/session/mod.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/session/mod.rs#L13) and used by HTTP in [plug-core/src/http/server.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/http/server.rs#L36). |
| Legacy SSE upstream transport | done | PR #35 adds `TransportType::Sse`, `LegacySseClientTransport` with HTTP→SSE auto-fallback, SSRF-hardened same-origin endpoint validation, redirect-disabled reqwest client, auth token Debug redaction, and import/export/doctor support. Integration tests cover explicit SSE, fallback, and notification forwarding. |
| OAuth / remote commercial MCP auth flows | missing | I found no implementation under `plug` or `plug-core`; current auth code is downstream bearer-token auth only in [plug-core/src/auth.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/auth.rs). |
| `sampling/createMessage` | done | PR #34 adds `DownstreamBridge` trait with `create_message()` across stdio, HTTP, and daemon IPC. Capability-gated. End-to-end integration tests for both transports. |
| `elicitation/create` | done | PR #34 adds `DownstreamBridge` trait with `create_elicitation()` across stdio, HTTP, and daemon IPC. Capability-gated. End-to-end integration tests for both transports. |
| `roots/list` forwarding | done | PR #32 adds `roots/list` reverse request, `roots/list_changed` notification handling, and union cache across stdio, HTTP, and daemon IPC in `plug-core/src/proxy/mod.rs`, `plug-core/src/http/server.rs`, `plug/src/daemon.rs`, and `plug/src/ipc_proxy.rs`. |
| MCP protocol-version request validation (downstream) | done | PR #31 adds `validate_protocol_version_for_post()` in `plug-core/src/http/server.rs`. Requires `MCP-Protocol-Version: 2025-11-25` on POST (except `InitializeRequest`). Returns 400 on missing/mismatched. |
| MCP protocol-version header on upstream requests | done | rmcp 1.1.0's `StreamableHttpClientTransport` automatically injects `mcp-protocol-version` after initialization using the negotiated version from the server's `InitializeResult`. Confirmed by repo-local confidence test `test_upstream_http_sends_protocol_version_header`. Source: rmcp `streamable_http_client.rs:385-387`. |
| Meta-tool mode / reduced discovery surface | done | Meta-tools are defined and enforced in [plug-core/src/proxy/mod.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/proxy/mod.rs#L2101) and [plug-core/src/proxy/mod.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/proxy/mod.rs#L1196). |
| Dead TUI dependency removal | done | The root manifest [Cargo.toml](/Users/robdezendorf/Documents/GitHub/plug/Cargo.toml) no longer includes `ratatui`, `crossterm`, or `color-eyre`. |

## What This Means For `docs/PLAN.md`

With PRs #35 and #36 merged, all major roadmap features are complete on `main`: Stream A protocol
correctness, roots forwarding, elicitation/sampling reverse-request forwarding, legacy SSE upstream
transport, and OAuth 2.1 + PKCE upstream authentication.

The one nuance: “daemon continuity recovery” is broader than what the code currently proves
(reconnect-based recovery for stdio-over-IPC clients, not full cross-transport session persistence).

## Remaining Open Work

All prior “minimum code gaps” from the original audit are resolved. No smaller items remain from the
original audit scope. Outstanding work is limited to OAuth follow-up polish and documentation hygiene
(tracked in `docs/PLAN.md`).
