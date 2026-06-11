# Current Plan

This document tracks the current product state and the next remaining work after the merged Phase
1-3 tranches and Stream A follow-ups.

## Current State

`plug` has completed the major stabilization, protocol-surface, protocol-correctness, and roots
forwarding work:

- stabilization and truth fixes
- notification forwarding (logging, tools/list_changed, resources/list_changed, prompts/list_changed, `AuthStateChanged` observability via logging-channel fan-out)
- progress and cancellation routing
- resources/prompts forwarding with subscribe/unsubscribe lifecycle
- completion forwarding across all three transports (stdio, HTTP, IPC)
- full task lifecycle support for tool-backed tasks across stdio, HTTP, and daemon IPC
- upstream task pass-through when supported, with local wrapper-mode execution otherwise
- structured output pass-through (outputSchema, structuredContent, resource_link)
- pagination
- capability synthesis (honest per-transport masking)
- tool semantics enrichment (`readOnlyHint`, `destructiveHint`, `idempotentHint`, `openWorldHint`, `taskSupport`) and canonicalized branding/display metadata
- meta-tool mode
- lazy tool discovery v2 with client-targeted lazy policy, OpenCode bridge search, bounded session working sets, and legacy `meta_tool_mode` compatibility
- end-to-end transport coverage
- daemon continuity recovery (stdio clients via IPC proxy reconnect)
- session-store abstraction seam and stateless design prep
- MCP-Protocol-Version header validation on downstream HTTP POST requests
- MCP-Protocol-Version header on upstream HTTP requests (provided by rmcp 1.7.0 after initialization)
- subscription pruning and rebind on route refresh (todo 039 resolved)
- roots forwarding with union cache across stdio, HTTP, and daemon IPC
- elicitation + sampling reverse-request forwarding across stdio, HTTP, and daemon IPC (PR #34)
- legacy SSE upstream transport with HTTP→SSE auto-fallback, SSRF hardening, and auth support (PR #35)
- OAuth 2.1 + PKCE upstream auth with credential storage (keyring + file fallback), background token refresh, AuthRequired health state, CLI auth commands, doctor checks, and correct HTTP auth header construction (PR #36, PR #47)
- mock OAuth provider integration coverage for metadata discovery, auth-code exchange persistence with state cleanup, token refresh persistence, and reconnect using refreshed credentials (PR #51)
- daemon IPC notification parity: progress, cancelled, and list_changed push forwarding across IPC (PR #38)
- zero-downtime token refresh: actual OAuth refresh_token exchange before reconnect, with injected-token skip path, shared auth-failure classification for refresh/reconnect decisions, cache reload error propagation, reconnect retry without re-refreshing after transient failure, non-IPC `AuthStateChanged` observability via logging fan-out, and a distinct refresh-exchange observability signal (PR #42, PR #43, PR #44, PR #45, PR #50)
- downstream OAuth remote server support and related config/runtime integration
- remote Claude connector follow-up fixes: protocol-version response adjustment, pagination cursor forwarding, larger page size, and connector stability improvements
- persisted token hydration before upstream connect
- downstream OAuth/operator hardening across discovery, metadata, challenge behavior, and non-interactive diagnostics
- topology-aware setup/link/repair/status flows that preserve and surface downstream transport/auth choices
- transport-aware live session inventory with explicit scope and availability across daemon proxy and downstream HTTP sessions
- daemon-owned downstream HTTP/HTTPS when the shared background service is running
- daemon-provided transport-complete live session truth in background-service mode
- pinned operator JSON contracts plus downstream HTTP inventory failure-path coverage
- performance and efficiency follow-through across auth-store reads, reload batching, SSE fanout, and config env traversal
- oversized result delivery hardening via artifact spillover, `plug://artifact/...` resource reads, daemon-IPC chunking, and artifact cache maintenance
- runtime-truth follow-up hardening across `status`, `tools`, `servers`, `clients`, and `doctor`
- explicit live reverse-request delivery failure handling for downstream HTTP sessions
- review-hardened task correctness around monotonic state transitions, reconnect-stable IPC ownership, and fail-closed pass-through dispatch
- downstream OAuth authorize-redirect allowlist and exposure-keyed secretless-OAuth guard, plus per-upstream operability metrics in `plug status --output json` (PR #60)
- first-class upstream catalog availability (`healthy | degraded | absent`) with last-known-good carry-forward for transient listing failures, closing the PR #58 subscription-rebind residual (PR #61)

## What Exists Today

The current product shape is:

- `plug connect` for stdio downstream clients
- `plug serve --daemon` / `plug start` for the shared background runtime (IPC + HTTP/HTTPS)
- `plug serve` for explicit standalone foreground HTTP/HTTPS serving
- shared upstream routing through `Engine`, `ServerManager`, and `ToolRouter`
- daemon-backed local sharing with reconnecting IPC proxy sessions and daemon-owned downstream HTTP
- transport-aware operator inventory that can rely on daemon truth directly when the background service is running, while still falling back cleanly during non-daemon foreground HTTP serving
- targeted notification fan-out to stdio, HTTP, and daemon IPC, including resource subscribe/unsubscribe and targeted resource update delivery
- meta-tool mode as an opt-in reduced discovery surface
- client-targeted lazy tool discovery, including OpenCode bridge mode where agents search with `plug__search_tools` and then call loaded routed tools directly
- downstream HTTP bearer token auth for non-loopback binding
- downstream OAuth mode for remote/server-card based authorization flows
- explicit runtime/auth/operator state vocabulary across `status`, `doctor`, `auth status`, `clients`, and `servers`
- single-flight reload application with bounded concurrent startup and safe shared registration
- pre-serialized broadcast SSE payloads for the hot HTTP notification path
- centralized env-reference traversal reused by config loading and doctor checks
- core MCP Tasks support as part of the routed downstream surface
- oversized tool/task result delivery that preserves success across transports via artifact spillover and daemon-IPC chunking for medium oversized inline responses

## Remaining Work

All major roadmap features are now implemented on `main`.
No required roadmap work remains for the current production-ready bar.

Optional future scope only:

- fully live runtime reconfiguration, if the product bar is expanded beyond the current release scope
- continuing optional operator/runtime polish now that daemon mode owns the primary shared runtime
- further low-priority simplification of internal reload/session/SSE helper structure
- move the ≥16MB artifact write off the async worker via `spawn_blocking` (requires making `ArtifactStore` shareable; deferred from PR #58)
- end-to-end metrics-recording test plus an RAII recording guard, and an operator-guide note on `degraded_since` vs. health divergence (deferred from PR #60)

## Designed-But-Deferred Program Phases

The 2026-06-10 operability/hardening program (`docs/plans/2026-06-10-002-feat-operability-hardening-program-plan.md`) scoped PR #60 to a bounded tranche and deliberately deferred larger items. Sequenced **item 3 → item 1 → item 2b**, plus the independent test-infra work:

- **Degraded-vs-absent core model** — ✅ done on `main` via PR #61 (see dated entry below). Closed the PR #58 subscription-rebind residual at the model level.
- **Transport `RequestDispatcher` + parity matrix** — 🟡 mostly done on `main`: the **`tools/call` slice** shipped via PR #63 and the **cross-transport parity gate now covers the entire method surface** via PR #64 (see dated entries below). PR #64 added parity rows for `tools/list`, `resources/{list,templates,read}`, `prompts/{list,get}`, `completion/complete`, and `resources/{subscribe,unsubscribe}`, enriched the mock upstream to drive real routed content, and consolidated the duplicated IPC result/error-encoding ladder into shared helpers (`ipc_ok` / `ipc_from_mcp_result`). The only remaining dispatcher item is the **`DownstreamTransport::Ipc` identity split** (KTD3) — deferred to its own PR because `NotificationTarget::Stdio` is the shared bridge/delivery key for both the in-process stdio path and daemon IPC across ~64 sites; the parity matrix now de-risks it. The full `ToolRouter` god-object decomposition also remains a separate, larger refactor.
- **ToolRouter god-object decomposition** — ✅ done on `main` via PR #65: `proxy/mod.rs` split 6,586 → 2,464 lines (63%) into six cohesive seam modules (`tests`, `handler`, `tasks`, `completion`, `subscriptions`, `catalog`), move-only and behavior-preserving. The coupled routing/notification/refresh core stays in `mod.rs` by design.
- **`DownstreamTransport::Ipc` identity split (KTD3)** — ✅ done on `main` via PR #66: IPC has its own `ipc:{id}` namespace + `NotificationTarget::Ipc` + `ipc_for_client` context; every daemon IPC site switched off the `Stdio` masquerade (in-process `StdioBridge` keeps `Stdio`); behavior-affecting but guarded by the parity matrix + IPC e2e delivery tests; two reviews returned zero findings.
- **Active upstream supervision (item 2b)** — ✅ done on `main` via PR #67: a bounded restart-on-degradation supervisor (`SupervisionConfig` + exponential backoff + `restart_count`/`last_restart_epoch_secs` in status JSON), triggered on sustained health failures or an open circuit. Four review-found storm vectors fixed before merge. **The operability/hardening program (items 1–4) is now fully landed.**
- **Test parallelism (U1/U2)** — ✅ done on `main` via PR #62 (see dated entry below): the suite now runs parallel in CI without `--test-threads=1`. Full `RuntimePaths` injection (so even the ~15 daemon/runtime tests run concurrently) remains a deferred enhancement.

## 2026-06-10 Transport Dispatcher — `tools/call` Slice (Program Item 1)

On 2026-06-10, `main` absorbed PR #63 — the first slice of item 1 (R8), scoped to the `tools/call` method family only:

- new `plug-core/src/dispatch` module: a `DownstreamContext` trait + `dispatch_tools_call` returning a `ToolCallOutcome` over `CallToolResult`/`CreateTaskResult`, owning the per-transport adapter shell once. The routing core is unchanged — planning corrected the program-plan premise that there were "three duplicated copies of tools/call" (the route was already shared; only the adapter shell + error encoding were duplicated)
- stdio/HTTP/IPC `tools/call` handlers now delegate to the dispatcher, with the task branch gated per-transport via `supports_tasks()` (stdio false). No product-surface behavior change; an 8-persona review returned zero production-code findings
- first end-to-end IPC test harness (none existed) + a cross-transport parity matrix asserting identical decoded results and error codes across the real stdio/HTTP/IPC transports — the recurring parity-drift bug class is now a CI gate
- empty-name response converged across transports (IPC's `INVALID_PARAMS` pre-check removed → all return `METHOD_NOT_FOUND`). Task-augmentation divergence (stdio rejects via rmcp validation; HTTP/IPC create a passthrough task) is intentional and pinned by the parity test

Deferred to follow-up: remaining method families migrate to the dispatcher in their own PRs (each extending the parity matrix); `DownstreamTransport::Ipc` identity split (IPC reuses the stdio identity today, KTD3); consolidating the duplicated mock `ServerConfig` fixture into `plug-test-harness`. Active upstream supervision (item 2b) remains the next sequenced phase after the dispatcher.

## 2026-06-10 Transport Parity Gate — Whole Method Surface (Program Item 1)

On 2026-06-10, `main` absorbed PR #64 — finishing the parity deliverable of item 1 (R8) across every method family (scope correction: unlike `tools/call`, the other families were already shared-core with thin shells, so the value was parity coverage + encode consolidation, not a trait migration):

- the cross-transport parity matrix now covers `tools/list`, `resources/{list,templates,read}` (+ unknown-uri error), `prompts/{list,get}` (+ unknown-prompt error), `completion/complete`, and the `resources/{subscribe,unsubscribe}` lifecycle — each asserting identical decoded results + error codes across the real stdio/HTTP/IPC transports. The harness was generalized to method-generic drivers (`parity_{stdio,http,ipc}_call` + `assert_parity`) normalizing to a canonicalized `MethodOutcome`; the existing `tools/call` rows pass unchanged (characterization guard)
- the mock upstream (`plug-test-harness/src/bin/mock-server.rs`) gained flag-gated prompts / completion / resource-template handlers so parity rows drive real routed content, not empty-list agreement
- the duplicated per-arm IPC `to_value → SERIALIZE_ERROR` ladder was consolidated into two shared helpers (`ipc_ok` / `ipc_from_mcp_result`) — behavior-preserving, proven by the matrix staying decoded-identical plus a helper unit test. An 8-persona review returned zero production-code findings; six test-quality fixes were applied
- empty-success encodings (stdio `()`, HTTP `EmptyResult`, IPC `json!({})`) proven equivalent through the parity normalizer

Deferred to follow-up: the `DownstreamTransport::Ipc` identity split (KTD3) — investigation showed `NotificationTarget::Stdio` is the shared bridge/delivery key for both the in-process stdio path and daemon IPC across ~64 sites, with subscription-rebind reconstructing `Stdio` targets on route refresh; a full split rewires notification delivery + reconnect-stable ownership, and a partial split risks orphaning working sets on reconnect, so it ships separately (now de-risked by the parity matrix). The `ToolRouter` god-object decomposition and active upstream supervision (item 2b) remain the next program phases.

## 2026-06-10 Parallel Test Suite (Program Item 4)

On 2026-06-10, `main` absorbed PR #62:

- the workspace test suite now runs with parallel threads in CI (`--test-threads=1` removed from both CI jobs and the docs). The daemon/ipc/runtime tests that share the process-global runtime-paths slot are unified behind one shared `daemon::runtime_paths_test_lock()`, so they serialize among themselves while the other ~665 tests run in parallel
- the mock MCP server is pre-built once (`plug_test_harness::mock_server_bin()`) and exec'd directly instead of `cargo run` per spawn, so parallel tests don't contend on Cargo's target lock
- proven parallel-safe: `cargo test --workspace` green 11/11 consecutive local runs plus both CI test jobs (ubuntu + macos); wall-clock ~135s → ~45s (integration suite 67s → 13s)

Deferred (recorded in PR #62): full `RuntimePaths` injection — delete the global entirely and thread explicit paths through `run_daemon` and the client-discovery sites — so even the ~15 daemon/runtime tests run concurrently.

## 2026-06-10 Degraded-vs-Absent Availability (Program Item 3)

On 2026-06-10, `main` absorbed PR #61:

- `ServerManager`'s resource/template/prompt listers stop returning `Ok(Vec::new())` on timeout — they classify each per-server call as fresh or unavailable and carry last-known-good forward for an unavailable-but-routable upstream, so `refresh_tools` sees an unchanged URI set and its subscription prune/unsubscribe loop leaves a stalled server alone
- first-class `Availability { healthy | degraded | absent }` recomputed each refresh, surfaced additively (schema-stable) on `ServerStatus` / `plug status --output json`; a routable upstream that fails to list reports `degraded` (never falsely `healthy`), `absent` is reserved for upstreams not in the routed set
- closes the PR #58 subscription-rebind residual; review caught and fixed a real failing-with-no-cache misclassification before merge

Residuals (recorded in PR #61, none a blocker): shared listing-helper extraction, pre-existing `health` vs `availability` JSON casing, an availability-scoped degraded-since timestamp (tied to item 2b), `refresh_tools` single-flight, and template/prompt degraded-path integration coverage.

## 2026-06-10 Operability + Tunneled-OAuth Hardening

On 2026-06-10, `main` absorbed PR #60:

- closed the downstream OAuth open-redirector: `build_authorize_redirect` validates the requested `redirect_uri` against a loopback-default allowlist before issuing the authorization code, percent-encodes code/state, and logs rejections
- added an exposure-keyed secretless-OAuth guard: config validation rejects `auth_mode = "oauth"` without `oauth_client_secret` when reachable off-loopback (non-loopback bind *or* non-loopback `public_base_url`, e.g. a cloudflared tunnel) — the first cut keyed only on bind address and missed the tunnel; review caught it and the merged guard keys on exposure
- added per-upstream metrics to `plug status --output json` (call/error counts, last-latency-ms, degraded-since epoch, circuit-state label) with a stable always-present schema, zero-filled for known-but-idle servers

Residuals are tracked under Remaining Work above (allowlist migration for remote `redirect_uri`; e2e metrics-recording test + RAII guard; operator-guide degraded-vs-health note).

## 2026-06-10 Code-Review Stabilization Batch

On 2026-06-10, `main` absorbed PR #58, a batch of validated code-review fixes:

- daemon IPC `tools/call` forwards `_meta.progressToken` (was dropped on the non-task path), completing progress-routing parity with stdio
- per-server resource/prompt listing in `refresh_tools` is bounded by `call_timeout_secs`; a stalled upstream is skipped rather than freezing the catalog refresh
- `notification_refresh_in_progress` is cleared via an RAII drop guard with a backstop timeout, so a panic/cancellation cannot wedge `list_changed` delivery
- DashMap deadlock on expired-artifact reads fixed; `read_chunk_text` reads one chunk instead of the whole payload; per-spill prune scan removed
- `plug server edit --output json` performs the edit; `plug doctor` exits with its computed code
- dead `sighup_reload` / `resource_subscription_count` removed; stale `rmcp`/`serde` doc claims corrected

Two follow-ups were deliberately left out of scope and are tracked under Remaining Work above (subscription-rebind on listing timeout; artifact-write off-thread).

## 2026-04-24 Lazy Tool Discovery V2

On 2026-04-24, `main` absorbed PR #56:

- client-targeted lazy policy for native, bridge, and disabled modes
- OpenCode bridge search with `plug__search_tools` as the only bridge meta tool
- search-loaded routed tools that preserve native tool names for direct calls, approvals, and permissions
- bounded per-session working sets and targeted list-change notifications
- legacy `meta_tool_mode` retained as deprecated compatibility, separate from bridge mode

## 2026-03-22 Tasks Tranche

On 2026-03-22, `main` absorbed the core Tasks tranche and immediate review fixes:

- task-wrapped `tools/call` plus `tasks/list`, `tasks/get`, `tasks/result`, and `tasks/cancel`
- daemon IPC, HTTP, and stdio parity for task flows
- upstream task pass-through proof via dedicated task-native integration coverage
- metadata enrichment and branding follow-through that landed alongside the tranche
- fixes for the blocking review findings raised during `ce:review`

## 2026-03-16 Reconciliation Note

The previously working off-main runtime line was reconciled into `main` on 2026-03-16, verified in
an isolated `main` worktree with a passing full test suite, and then pushed as the new canonical
baseline.

## 2026-03-17 Operator Truth Expansion

On 2026-03-17, `main` absorbed the follow-on operator/runtime hardening work that:

- aligned `status`, `doctor`, `auth status`, `clients`, and `servers` around one explicit auth/runtime vocabulary
- preserved downstream topology choices through setup/link/repair flows
- introduced transport-aware live-session inventory with explicit availability/scope semantics
- added regression coverage for JSON operator contracts and downstream HTTP inventory failure paths

## 2026-03-17 Daemon-Owned HTTP Runtime

On 2026-03-17, `main` also moved the shared background service to the next architecture step:

- daemon mode now owns downstream HTTP/HTTPS as well as IPC proxy sessions
- daemon `ListLiveSessions` can report transport-complete truth directly
- runtime/operator surfaces trust daemon-provided complete inventory when it is available
- standalone `plug serve` remains as an explicit foreground mode rather than the primary runtime authority

## 2026-03-18 Performance And Runtime Truth Hardening

On 2026-03-18, `main` absorbed a focused hardening pass that:

- unified OAuth credential snapshot reads and reduced duplicated auth-store IO
- added fail-fast handling for dead HTTP reverse-request targets, then tightened send-time live-delivery failure handling
- batched reload startup with configured concurrency while serializing reload execution and protecting shared upstream registration
- coalesced health-triggered refresh work and deduplicated proactive recovery scheduling
- pre-serialized SSE notification payloads for HTTP fanout
- centralized config env traversal and reused it in doctor env inspection
- clarified operator runtime truth when the daemon is reachable but IPC/runtime inspection is unavailable
