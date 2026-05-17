# Plug Hardening Log

## 2026-05-17 Phase 1 - Low-risk wins

Shipped:

- Replaced the downstream HTTP initialize response JSON mutation with typed `InitializeResult::with_protocol_version(ProtocolVersion::V_2025_11_25)`. The response still advertises `2025-11-25`; the workaround implementation is gone.
- Updated patch/stable dependency paths:
  - `reqwest 0.13.2 -> 0.13.3`
  - `tokio 1.52.1 -> 1.52.3`
  - `tower-http 0.6.8 -> 0.6.10`
  - `notify 7.0.0 -> 8.2.0`
  - `notify-debouncer-mini 0.5.0 -> 0.7.0`
  - `axum-server 0.7.3 -> 0.8.0`
- Cleared the `instant` advisory by moving off `notify-types 1.x`.
- Cleared the `rustls-pemfile` advisory by moving to `axum-server 0.8.0`, which uses `rustls-pki-types` directly.
- Hardened `test_stdio_timeout_reconnects_cleanly` so it waits for the background reconnect outcome instead of relying on a fixed 200 ms sleep.

Tests and checks:

- `cargo test -p plug-core http::server::tests::initialize_response_contains_server_info -- --nocapture` passed.
- `cargo test -p plug-core --test integration_tests test_stdio_timeout_reconnects_cleanly -- --test-threads=1 --nocapture` passed after the test hardening.
- `cargo fmt --all -- --check` passed.
- `cargo clippy --workspace -- -D warnings` passed.
- `cargo test --workspace -- --test-threads=1` passed: 142 `plug` tests, 424 `plug-core` tests, 41 integration tests, and doc-test/no-test crates.
- `cargo deny check advisories` now reports only `serde_yml` (`RUSTSEC-2025-0068`), which is the next Phase 2 critical item and is not accepted as a deferral.

Runtime smoke:

- `./target/debug/plug status --output json` reported the live daemon running with `transport_complete` inventory, 10 daemon-proxy sessions, downstream OAuth at `https://plug.plugtunnel.com/mcp`, and all configured upstream servers healthy.
- `./target/debug/plug clients --output json` reported live Claude Code and Codex CLI sessions. Other detected/linked clients were not actively connected at the time of the check.
- I did not non-interactively drive GUI clients through tool/resource/prompt/sampling/elicitation flows in this phase. That remains a manual gate requirement for a launch cut, not a code deferral.

Surprises:

- The reconnect integration test exposed an actual race in the test assumption: an immediate retry can still hit the old timed-out stdio process while background reconnect is in flight. The production behavior remains background reconnect; the test now asserts eventual success after reconnect instead of assuming a fixed delay.

Deferred:

- None accepted. `serde_yml` remains open only because it is Phase 2 item #4.

## 2026-05-17 Phase 2 U2 - YAML dependency replacement

Shipped:

- Replaced the unmaintained `serde_yml 0.0.12` dependency with `serde_norway 0.9.42`.
- Updated every YAML call site:
  - client config detection and Goose YAML merge in `plug/src/commands/clients.rs`
  - Goose export in `plug-core/src/export.rs`
  - Goose import in `plug-core/src/import.rs`
  - client YAML validation in `plug-core/src/doctor.rs`
- Added behavior tests for Goose YAML export shape, Goose import parsing, skipping Plug's own Goose entry during import, and preserving existing Goose extensions while merging Plug's config.

Tests and checks:

- `cargo test -p plug clients::tests::merge_yaml_config_preserves_existing_goose_extensions -- --nocapture` passed.
- `cargo test -p plug-core export::tests::export_goose_http_yaml_has_expected_shape -- --nocapture` passed.
- `cargo test -p plug-core import::tests::parse_goose_yaml_imports_servers_and_skips_plug -- --nocapture` passed.
- `cargo test --workspace -- --test-threads=1` passed: 143 `plug` tests, 426 `plug-core` tests, 41 integration tests, and doc-test/no-test crates.
- `cargo fmt --all -- --check` passed.
- `cargo clippy --workspace -- -D warnings` passed.
- `cargo deny check advisories` passed with `advisories ok`.

Surprises:

- None. `serde_norway` preserved the `serde_yml` value/mapping API shape closely enough that the code change stayed mechanical and the user-authored YAML behavior remained explicit in tests.

Deferred:

- None for U2.

## 2026-05-17 Phase 2 U3 - `rmcp` 1.7.0 upgrade

Shipped:

- Upgraded the workspace `rmcp` dependency from `1.5.0` to `1.7.0`.
- Preserved Plug's custom downstream HTTP server, session handling, routing, task ownership, OAuth, legacy SSE upstream transport, and daemon IPC code.
- Updated downstream HTTP error-response construction for the `rmcp 1.7.0` API change: `ServerJsonRpcMessage::error(...)` now takes `Option<RequestId>`, so routed HTTP errors pass `Some(request_id)`.
- Added `http::server::tests::routed_http_error_preserves_request_id` to prove routed JSON-RPC errors still carry the downstream request id after the SDK API change.

Tests and checks:

- `cargo check --workspace` passed.
- `cargo test -p plug-core http::server::tests::routed_http_error_preserves_request_id -- --nocapture` passed.
- `cargo fmt --all -- --check` passed.
- `cargo clippy --workspace -- -D warnings` passed.
- `cargo test --workspace -- --test-threads=1` passed: 143 `plug` tests, 427 `plug-core` tests, 41 integration tests, and doc-test/no-test crates.
- `cargo deny check advisories` passed with `advisories ok`.

Runtime smoke:

- `./target/debug/plug status --output json` reached the live daemon, reported `transport_complete` inventory, 10 daemon-proxy sessions, downstream OAuth at `https://plug.plugtunnel.com/mcp`, and all configured upstream servers healthy.
- `./target/debug/plug clients --output json` reported live Claude Code and Codex CLI sessions through daemon IPC. Factory and Google Antigravity are linked but not live; Cursor, Gemini CLI, and OpenCode were detected but not linked/live in this smoke output; VS Code Copilot, Windsurf, and Zed were not detected.
- I did not restart the live daemon for this smoke because Plug operational guidance says not to restart daemon/runtime processes unless explicitly asked. This validates CLI/operator compatibility with the current live daemon, while the full test suite validates the rebuilt `rmcp 1.7.0` code paths.

Surprises:

- The `rmcp` upgrade was narrower than expected. No model-shape changes affected Plug's current prompt/resource fixtures, OAuth code, task code, or mock server.
- `ping` is handled successfully by the current HTTP path, so the new request-id regression targets a real routed resource-read error instead of assuming `ping` is unsupported.

Deferred:

- SDK Streamable HTTP server/session-store adoption remains deferred to a separate transport redesign. Reason: this hardening pass explicitly preserves Plug's custom downstream HTTP server and the audit classifies SDK adoption as medium/high wire-risk for clients. Owner: Rob. Re-review date: 2026-07-01 or when the SSE/stateless transport tranche is complete.

## 2026-05-17 Phase 2 gate - Dependency hygiene

Phase 2 is complete:

- U2 replaced `serde_yml`; no direct RustSec advisory remains.
- U3 upgraded `rmcp` to `1.7.0` without adopting SDK HTTP server internals.
- Full workspace tests, clippy, formatter check, and advisory check pass.
- Runtime smoke reached the live daemon and two active client families: Claude Code and Codex CLI.

Deferred:

- Manual GUI-client exercise across every advertised client remains a launch-cut gate, not a Phase 2 blocker. The currently live machine state only exposed Claude Code and Codex CLI sessions non-interactively.

## 2026-05-17 Phase 3 U4 - SSE resumability

Shipped:

- Added session-owned monotonic SSE event IDs and a bounded per-session replay buffer for downstream HTTP sessions.
- Wired `Last-Event-ID` into the HTTP SSE reconnect path so missed notifications replay when the client reconnects with a cursor.
- Kept first-connect behavior compatible: sessions without `Last-Event-ID` drain pending notifications but do not replay old buffered history.
- Made upstream reverse requests replayable across a transient SSE disconnect, then remove their replay entries when the downstream client responds or the request times out.
- Preserved Plug's custom HTTP/session stack; this does not adopt the `rmcp` Streamable HTTP server.

Tests and checks:

- `cargo test -p plug-core session::stateful::tests -- --nocapture` passed, including replay, pending-drain, and reverse-request replay-key pruning tests.
- `cargo test -p plug-core http::sse::tests -- --nocapture` passed.
- `cargo test -p plug-core http::server::tests::last_event_id_replays_missed_http_sse_notifications -- --nocapture` passed.
- `cargo test -p plug-core http::server::tests::queued_reverse_request_replays_on_reconnect -- --nocapture` passed.
- `cargo fmt --all -- --check` passed.
- `cargo clippy --workspace -- -D warnings` passed.
- `cargo test --workspace -- --test-threads=1` passed: 143 `plug` tests, 432 `plug-core` tests, 41 integration tests, and doc-test/no-test crates.
- `cargo deny check advisories` passed with `advisories ok`.

Surprises:

- The reverse-request path needed replay-key cleanup, not just generic notification buffering. Without that, a reconnect after a completed sampling/elicitation call could replay a stale request.

Deferred:

- Stateless/sessionless transport migration remains deferred. Reason: the accepted transport SEPs point away from long-lived session-bound semantics, but Plug's current public HTTP surface is still stateful and existing clients depend on that model. Owner: Rob. Re-review date: 2026-07-01 or when Phase 4 transport alignment starts.

## 2026-05-17 Phase 3 U5 - Resource subscribe over daemon IPC

Shipped:

- Added a `ResourceUpdatedNotification` IPC frame carrying serialized MCP `ResourceUpdatedNotificationParam`.
- Routed daemon-backed `resources/subscribe` and `resources/unsubscribe` into the existing `ToolRouter` subscription registry instead of creating a daemon-local registry.
- Forwarded targeted `ResourceUpdated` control notifications through daemon IPC only to the matching stdio session.
- Removed the daemon capability mask that hid `resources.subscribe` from IPC clients once upstreams support subscriptions.
- Cleaned resource subscriptions during daemon session replacement, explicit deregistration, and connection-drop auto-deregistration.
- Extended the test harness mock server with an opt-in subscribable resource fixture.

Tests and checks:

- `cargo test -p plug-core ipc::tests::response_serialization_round_trip -- --nocapture` passed.
- `cargo test -p plug resource_updated -- --nocapture` passed.
- `cargo test -p plug ipc_proxy::tests::daemon_backed_proxy_forwards_resource_subscribe_updates -- --nocapture` passed.
- `cargo fmt --all -- --check` passed.
- `cargo clippy --workspace -- -D warnings` passed.
- `cargo test --workspace -- --test-threads=1` passed: 145 `plug` tests, 432 `plug-core` tests, 41 integration tests, and doc-test/no-test crates.
- `cargo deny check advisories` passed with `advisories ok`.

Surprises:

- The safest implementation was deletion of the old capability mask plus reuse of the router-owned subscription cleanup paths. The daemon did not need a second registry.

Deferred:

- None for U5.

## 2026-05-17 Phase 3 U6 - Operator trust inventory

Shipped:

- Added IPC-level `source` and `trust` metadata for every daemon-listed tool.
- Classified servers as Plug-internal, configured local process, configured remote server, or runtime unknown based on live config and server id.
- Exposed non-secret source metadata through `plug tools --output json` per tool.
- Exposed the same source/trust inventory through `plug servers --output json` per live server, and as additive `server_inventory` metadata when the daemon is unavailable and the CLI falls back to config-only inspection.
- Preserved existing config-only JSON `servers` shape so automation that reads configured servers does not have to move immediately.

Tests and checks:

- `cargo test -p plug-core ipc::tests -- --nocapture` passed, including `source_and_trust_metadata_do_not_expose_secrets`.
- `cargo fmt --all -- --check` passed.
- `cargo clippy --workspace -- -D warnings` passed.
- `cargo test --workspace -- --test-threads=1` passed: 145 `plug` tests, 433 `plug-core` tests, 41 integration tests, and doc-test/no-test crates.
- `cargo deny check advisories` passed with `advisories ok`.

Surprises:

- The CLI server view already had both live and config-only modes, so the safest JSON surface was additive metadata rather than replacing the existing `servers` config map in fallback mode.

Deferred:

- Risk-level annotation semantics stay in U7. This tranche only identifies where a tool/server comes from and which trust boundary it crosses.

## 2026-05-17 Phase 3 U7 - Risky-tool annotation inventory

Shipped:

- Added operator-only risk metadata that separates `upstream_declared`, `plug_inferred`, and `effective` tool annotation hints.
- Preserved this metadata in the router snapshot while leaving the MCP `tools/list` wire payload unchanged.
- Added `has_conflict` so operators can identify tools where upstream declarations disagree with Plug's normalized safety hints.
- Exposed the risk inventory through daemon `ListTools` IPC and `plug tools --output json` alongside the existing source/trust metadata.

Tests and checks:

- `cargo test -p plug-core ipc::tests -- --nocapture` passed, including `tool_risk_metadata_separates_declared_inferred_and_effective_hints`.
- `cargo check --workspace` passed.

Surprises:

- The existing normalization path intentionally overwrites wrong upstream hints before exposing tools to clients. Preserving the pre-normalized hints in router metadata was the clean way to reduce operator overconfidence without changing client-facing behavior.

Deferred:

- Text-mode risk summaries remain deferred. Reason: the launch-critical need is machine-readable inventory; adding human summaries belongs with the operator guide/UI polish pass. Owner: Rob. Re-review date: 2026-06-15 or during Phase 6 documentation.

## 2026-05-17 Phase 3 U8 - Trace correlation

Shipped:

- Added W3C/OpenTelemetry-shaped 32-hex trace IDs for downstream tool-call contexts.
- Preserved valid downstream HTTP `traceparent` IDs and accepted `x-plug-trace-id` as an operator/debug fallback.
- Carried trace IDs through `DownstreamCallContext`, active tool routing, retry/reconnect logs, native upstream task creation, and background task execution logs.
- Added trace IDs to `ToolCallStarted` and `ToolCallCompleted` engine events so observability consumers can correlate request, router, upstream call, and completion.

Tests and checks:

- `cargo test -p plug-core trace_id -- --nocapture` passed.
- `cargo test -p plug-core downstream_context_preserves_supplied_http_trace_id -- --nocapture` passed.
- `cargo check --workspace` passed.

Surprises:

- The useful correlation boundary is the router call context, not the HTTP handler alone. Stdio clients and daemon-backed calls need generated trace IDs just as much as HTTP clients need propagated IDs.

Deferred:

- Formal SEP-2243 `Mcp-Method` / `Mcp-Name` validation and response/header emission remains Phase 4 #11. Reason: this tranche establishes end-to-end correlation without adding request rejection behavior. Owner: Rob. Re-review date: 2026-05-24 or at Phase 4 start.

## 2026-05-17 Phase 3 Gate - Multiplexor correctness

Shipped:

- Completed the Phase 3 launch-critical multiplexor correctness tranche: SSE replay, daemon IPC resource subscription parity, operator trust inventory, risk provenance inventory, and trace correlation.
- Reinstalled the current workspace binary with `./scripts/dev-reinstall.sh --quick` so the shared daemon smoke test exercised the just-built code, not the previously installed daemon binary.
- Restarted the shared daemon from the normalized `~/.local/bin/plug -> ~/.cargo/bin/plug` install path.

Tests and checks:

- `cargo fmt --all -- --check` passed.
- `cargo clippy --workspace -- -D warnings` passed.
- `cargo deny check advisories` passed with `advisories ok`.
- `cargo test --workspace -- --test-threads=1` passed after U8: 145 `plug` tests, 438 `plug-core` tests, 41 integration tests, and doc-test/no-test crates.

Runtime smoke:

- `plug status --output json` reported `runtime_available: true`, 9 daemon-proxy client sessions, and 11 healthy upstream servers: context7, exa, figma, imessage, krisp, notion, oura, slack, svelte, todoist, and workspace.
- `plug clients --output json` reported live Claude Code and Codex CLI sessions after daemon restart. Other linked/detected clients were not actively connected during this non-interactive smoke.
- `plug tools --output json` reported `status_source: live_daemon`, 339 tools, 11 server groups, and live tool inventory entries with non-null `source`, `trust`, and `risk` metadata.

Surprises:

- `cargo run -p plug -- start` initially restarted the already-installed daemon binary through `~/.local/bin/plug`, so the first smoke test still showed old IPC defaults for tool metadata. The project reinstall script is required before live daemon smoke when validating daemon IPC schema changes.

Deferred:

- Full GUI-client exercise across every linked client remains a launch-cut manual gate. Reason: only Claude Code and Codex CLI were live and controllable non-interactively in this session. Owner: Rob. Re-review date: 2026-05-24 or before public launch.

## 2026-05-17 Phase 4 U9 - SEP-2243 HTTP header standardization

Shipped:

- Added a shared MCP HTTP header helper for standard `Mcp-Method` and `Mcp-Name` mirroring.
- Downstream HTTP now validates present `Mcp-Method` / `Mcp-Name` headers against the JSON-RPC body and rejects mismatches with HTTP 400 plus JSON-RPC error code `-32001` (`HeaderMismatch`).
- Missing SEP-2243 headers remain accepted for current clients because Plug still advertises `2025-11-25`, and the SEP gates required headers on the protocol version that introduces them.
- Streamable HTTP upstream requests now receive mirrored `Mcp-Method` / `Mcp-Name` headers through the `rmcp` custom-header map.
- Legacy SSE upstream POSTs now emit the same mirrored headers.

Tests and checks:

- `cargo test -p plug-core mcp_http_headers -- --nocapture` passed.
- `cargo test -p plug-core post_rejects_mismatched -- --nocapture` passed.
- `cargo test -p plug-core explicit_sse_upstream_connects_and_routes_tool_calls -- --nocapture` passed and asserts the upstream received `Mcp-Method: tools/call` and `Mcp-Name: echo`.
- `cargo fmt --all -- --check` passed.
- `cargo clippy --workspace -- -D warnings` passed.

Surprises:

- Strict missing-header rejection would be a client-visible wire break today. The implementation validates and emits the Final SEP headers without forcing current clients to send them before a future protocol-version bump.

Deferred:

- Custom `x-mcp-header` / `Mcp-Param-*` mirroring remains deferred. Reason: the execution prompt scoped Phase 4 #11 to `Mcp-Method` / `Mcp-Name`; parameter-level mirroring needs tool-schema validation and could affect tool visibility. Owner: Rob. Re-review date: 2026-06-15 or during the next protocol-version alignment pass.

## 2026-05-17 Phase 4 U10 - Server card alignment

Shipped:

- Added the current SEP-2127 draft discovery path `/.well-known/mcp-server-card`.
- Preserved the existing `/.well-known/mcp.json` path as a compatibility alias.
- Replaced Plug's earlier dynamic/full-vs-minimal card with a static server-card shape: `$schema`, `name`, `version`, `description`, `title`, `websiteUrl`, `repository`, and `remotes`.
- Moved authentication discoverability into the remote `headers` entry for protected deployments instead of exposing Plug-specific `auth_required` fields.
- Stopped exposing dynamic tool counts and upstream server names in the public card; this matches the draft's guidance that primitives are dynamic and should be discovered through protocol list operations after connection.
- Added CORS and cache headers expected for browser/discovery consumers.

Tests and checks:

- `cargo test -p plug-core server_card -- --nocapture` passed, covering the new path, legacy alias, external-origin access, protected bearer mode, protected OAuth mode, and authenticated requests that must not expand into dynamic inventory.

Surprises:

- SEP-2127 is still Draft and the path moved from the older `/.well-known/mcp.json` assumption to `/.well-known/mcp-server-card`. The safest compatibility posture is aliasing rather than removing the old path.

Deferred:

- Canonical server-card `name`, `websiteUrl`, and `repository` may need to change after the Phase 5 namespace decision. Reason: the current workspace metadata still points at `plug-mcp/plug`, but Phase 5 explicitly reserves that repository/org choice for Rob. Owner: Rob. Re-review date: Phase 5 item #15.

## 2026-05-17 Phase 4 U11 - Auth alignment with finalized SEPs

Shipped:

- Added RFC 9728 protected-resource metadata at `/.well-known/oauth-protected-resource` and `/.well-known/oauth-protected-resource/mcp`.
- Updated downstream OAuth `WWW-Authenticate` challenges so `resource_metadata` points at protected-resource metadata, not authorization-server metadata.
- Added protected-resource metadata fields for `resource`, `authorization_servers`, `scopes_supported`, and `bearer_methods_supported`.
- Filtered `offline_access` out of resource-server scopes in protected-resource metadata and challenges, matching SEP-2207 guidance.
- Advertised `client_credentials` in authorization-server metadata only when the downstream OAuth config has a confidential client secret.
- Added downstream `grant_type=client_credentials` support for confidential clients. Issued tokens are bearer access tokens only; no refresh token is issued for M2M credentials.
- Left SEP-991 URL client metadata documents unadvertised because Plug does not yet fetch or validate URL-formatted client IDs and should not claim support before an SSRF-safe trust policy exists.

Tests and checks:

- `cargo test -p plug-core downstream_oauth -- --nocapture` passed, including the downstream OAuth protected discovery card integration test after updating it for static server cards and protected-resource metadata.
- `cargo test -p plug-core oauth_ -- --nocapture` passed, including protected-resource metadata, client-credentials token issuance, authorization-code flow, refresh flow, and upstream OAuth recovery coverage.
- `cargo fmt --all -- --check` passed.
- `cargo clippy --workspace -- -D warnings` passed.

Surprises:

- The old OAuth discovery integration test encoded a now-obsolete `auth_required` server-card field. The server-card U10 change correctly moved auth discoverability into `remotes[].headers` and the protected-resource metadata endpoint.

Deferred:

- SEP-991 URL client metadata documents remain deferred. Reason: implementing them requires outbound metadata fetches from user-supplied client IDs plus SSRF/rate-limit/cache/trust-policy work; advertising support before that would be false. Owner: Rob. Re-review date: 2026-06-15 or when a real remote client needs URL client metadata.
- SEP-1932 DPoP and SEP-1933 Workload Identity remain deferred. Reason: both were draft SEPs in the audit and should not become Plug-specific public protocol extensions. Owner: Rob. Re-review date: when either SEP becomes Final/Accepted.

## 2026-05-17 Phase 4 U12 - Opt-in stdio sandboxing

Shipped:

- Added per-stdio-server `sandbox` config with `enabled`, `allow_network`, `allow_read`, `allow_write`, and `profile_path`.
- When `sandbox.enabled = true` on macOS, Plug launches the upstream child process through `/usr/bin/sandbox-exec`.
- If `profile_path` is set, Plug uses that operator-supplied profile file.
- If `profile_path` is absent, Plug generates a deny-by-default macOS sandbox profile with common system read access, configured read/write allowlists, optional network access, and no shell interpolation.
- Rejected sandbox config on non-stdio transports during config validation.
- Preserved default behavior: no existing upstream is sandboxed unless explicitly opted in.

Tests and checks:

- `cargo test -p plug-core generated_stdio_sandbox_profile -- --nocapture` passed.
- `cargo test -p plug-core validate_sandbox -- --nocapture` passed.
- `cargo clippy --workspace -- -D warnings` passed.

Surprises:

- Process resource limits are not safely enforceable through the existing `tokio::process::Command` path without unsafe pre-exec hooks or a separate supervisor. The first launch-ready tranche should enforce filesystem/network policy and document the process-limit gap honestly.

Deferred:

- Cross-platform Linux sandbox enforcement remains deferred. Reason: this pass uses the platform primitive available on Rob's current machine; Linux needs a separate design around Bubblewrap/firejail/cgroups and installer prerequisites. Owner: Rob. Re-review date: 2026-06-15.
- CPU/memory/process-count limits remain deferred. Reason: enforcing them safely requires a supervisor or platform-specific process-control layer that would materially expand scope. Owner: Rob. Re-review date: 2026-06-15.

## 2026-05-17 Phase 4 gate

Checks:

- `cargo test --workspace -- --test-threads=1` passed: `plug` unit tests, `plug-core` unit tests (449), integration tests (41), `plug-test-harness`, mock server, and doc tests.
- `cargo deny check advisories` passed with `advisories ok`.
- `cargo clippy --workspace -- -D warnings` passed.
- `./scripts/dev-reinstall.sh --quick` rebuilt and replaced `/Users/robdezendorf/.cargo/bin/plug`, with `/Users/robdezendorf/.local/bin/plug` pointing at it.
- Live smoke after restart showed the daemon running with 9 daemon-proxy sessions, 11 healthy upstreams, 339 tools, and live Claude Code plus Codex CLI sessions.
- `/.well-known/mcp-server-card` returned the static server-card shape with `Authorization` listed as a required remote header for the protected local deployment.

Surprises:

- `plug start` returned a readiness-timeout error while the daemon continued booting; subsequent status showed the daemon healthy. Treat this as an operational sharp edge to revisit if it repeats, but it did not block the Phase 4 gate.

## 2026-05-17 Phase 5 U13 - Distribution namespace and install surface

Shipped:

- Recorded the required owner decision to standardize public distribution on the neutral `plug-mcp` namespace.
- Renamed the publishable CLI package from `plug` to `plug-mcp` while keeping the installed binary name `plug`.
- Added workspace `homepage` metadata and inherited repository/homepage metadata into the published crates.
- Marked `plug-test-harness` as `publish = false` and `dist = false` so release plans do not publish the mock test server.
- Replaced README `cargo install plug` with `cargo install plug-mcp --locked`, avoiding the occupied crates.io `plug` package.
- Removed the broken `get.plug.sh` installer path from README and documented the cargo-dist release installer artifact `plug-mcp-installer.sh`.
- Fixed `dist-workspace.toml` for cargo-dist 0.31.0 by changing the member to `cargo:plug`, enabling Homebrew publish jobs, and adding `profile.dist`.
- Updated CI/release workflow package selectors from `-p plug` to `-p plug-mcp`.

Checks:

- `cargo info plug` outside the workspace resolves to the unrelated `hecrj/plug` IPC crate.
- `cargo info plug-mcp` outside the workspace currently reports no such crate, so the name is available as checked.
- `curl -I https://get.plug.sh` still returns Cloudflare 525; README no longer references it.
- `cargo check --workspace` passed after the package rename.
- `dist plan --no-local-paths --allow-dirty` passed and now reports a single `plug-mcp` app, `plug-mcp-installer.sh`, `plug.rb`, and target archives containing the `plug` binary.
- `dist build --artifacts=global --allow-dirty` produced `source.tar.gz`, `plug-mcp-installer.sh`, `plug.rb`, and `sha256.sum`.
- `dist build --artifacts=local --target aarch64-apple-darwin --allow-dirty` produced `plug-mcp-aarch64-apple-darwin.tar.gz`; extracting it and running the binary reported `plug 0.1.0`.
- `cargo package -p plug-core --allow-dirty` verified `plug-core`.
- `cargo install --path plug --force --locked` installed package `plug-mcp` and replaced the local `plug` binary successfully.

Surprises:

- `dist-workspace.toml` was not merely stale; cargo-dist 0.31.0 could not parse the old unprefixed member syntax.
- The first cargo-dist plan accidentally included `plug-test-harness` because it has a binary target. Marking it non-publishable/non-distable removed it from the release plan.
- `cargo package -p plug-mcp` cannot verify until `plug-core 0.1.0` exists on crates.io. This is the normal publish order for the split core/CLI crates, but it means `plug-mcp` package verification is not fully self-contained before the first `plug-core` publish.

Deferred:

- Creating or migrating the GitHub org/repo `plug-mcp/plug` and tap repo `plug-mcp/homebrew-tap` remains an owner action. Reason: GitHub organization creation is outside repo-local code changes. Owner: Rob. Re-review date: before first public release tag.
- Publishing `plug-core 0.1.0` and then `plug-mcp 0.1.0` to crates.io remains an owner action. Reason: crate publishing requires Rob's crates.io credentials and should happen after namespace migration is complete. Owner: Rob. Re-review date: before README install commands are treated as live public commands.

## 2026-05-17 Phase 5 gate

Checks:

- `cargo test --workspace -- --test-threads=1` passed after the package rename: 449 `plug-core` tests, 41 integration tests, 145 `plug` binary crate tests, `plug-test-harness`, mock server, and doc tests.
- `cargo deny check advisories` passed with `advisories ok`.
- `cargo clippy --workspace -- -D warnings` passed.
- Live smoke with the installed `plug` binary reported the daemon running, 9 daemon-proxy sessions, 11 healthy upstreams, 339 tools, and live Claude Code plus Codex CLI sessions.
- `/.well-known/mcp-server-card` returned the `io.github.plug-mcp/plug` card with protected `Authorization` remote header metadata.

Deferred:

- The advertised public install commands are release-ready but not yet globally live until Rob completes the org/tap/crates.io owner actions recorded in Phase 5 U13.

## 2026-05-17 Phase 6 U14 - External-user documentation

Shipped:

- Updated README documentation links and install principle copy to use the `plug-mcp` distribution namespace.
- Reconciled `docs/MCP-SPEC.md` with the current strict `2025-11-25` protocol-version stance, typed initialize response construction, Streamable HTTP downstream surface, legacy SSE upstream-client scope, static server-card discovery path, and stateless/sessionless future work.
- Refreshed `docs/USERS.md` from the old `fanout` naming to Plug, current Homebrew install command, `plug setup --yes`, `plug start`, and current server-management command names.
- Added `docs/OPERATOR-GUIDE.md` covering runtime files, TLS/non-loopback rules, downstream auth, upstream OAuth, observability, operator inventory, opt-in stdio sandboxing, and release-operation checks.
- Added `SECURITY.md` with private GitHub advisory reporting guidance, supported-version posture, security model, and secret-redaction expectations.
- Added `CONTRIBUTING.md` with workspace layout, required checks, distribution checks, multiplexor mental model, and PR expectations.
- Updated `docs/audit-2026-05-17.md` with the Phase 6 external-user documentation status row.

Checks:

- Stale public-doc scan for `fanout`, broken `get.plug.sh`, old `cargo install plug`, old `brew install plug && plug`, old `--headless`, and obsolete protocol-version compatibility claims found no live README/MCP-SPEC/USERS/operator/security/contributing issues. Remaining matches are historical audit evidence and the intentional `cargo install plug-mcp` command.
- `cargo test --workspace -- --test-threads=1` passed: 449 `plug-core` tests, 41 integration tests, 145 `plug` binary crate tests, `plug-test-harness`, mock server, and doc tests.
- `cargo deny check advisories` passed with `advisories ok`.
- `cargo clippy --workspace -- -D warnings` passed.
- Live smoke with the installed `plug` binary reported the daemon running, 9 daemon-proxy sessions, 11 healthy upstreams, 339 tools, and live Claude Code plus Codex CLI sessions.
- `/.well-known/mcp-server-card` returned the static `io.github.plug-mcp/plug` card with `2025-11-25` as the supported protocol version and protected `Authorization` remote header metadata.
- An unauthenticated `/mcp` request returned `401 Unauthorized` with `WWW-Authenticate` pointing at `https://plug.plugtunnel.com/.well-known/oauth-protected-resource`.

Surprises:

- `docs/USERS.md` was still entirely branded as the old `fanout` product. README linked to it as a current doc, so leaving it historical would have undercut the external-user readiness pass.

Deferred:

- GitHub private vulnerability reporting cannot be fully verified until the public `plug-mcp/plug` repository exists and private reporting is enabled. Reason: this is controlled by GitHub repository settings after the namespace migration. Owner: Rob. Re-review date: before first public release tag.
- README install commands remain release-ready but not globally live until the Phase 5 owner actions are complete. Reason: GitHub org/tap creation and crates.io publish require Rob-owned credentials. Owner: Rob. Re-review date: before first public release tag.

## 2026-05-17 Owner-action follow-up

Superseded on 2026-05-17: public launch no longer requires creating a separate `plug-mcp` GitHub organization. The launch namespace is the existing public `cyberpapiii/plug` repository, while the crates.io package name remains `plug-mcp` because `plug` is occupied.

Completed:

- Pushed hardened `main` to the current public remote `https://github.com/cyberpapiii/plug` (`39831d7..43b2c86`).
- Enabled GitHub private vulnerability reporting on the current `cyberpapiii/plug` repository through the repository REST API. Verification returned `{"enabled":true}`.
- Verified `plug-core 0.1.0` packaging and publish dry-run succeed.

Blocked:

- Publishing `plug-core 0.1.0` and `plug-mcp 0.1.0` is blocked because this machine has no crates.io token configured (`cargo owner --list ...` reports `no token found`; `CARGO_REGISTRY_TOKEN` is unset; no `~/.cargo/credentials*` file exists).
- `plug-mcp 0.1.0` publish dry-run remains blocked until `plug-core 0.1.0` exists on crates.io, which is the expected publish-order dependency.

Next owner inputs required:

- Provide a crates.io API token with publish rights for the first release, then publish `plug-core` before `plug-mcp`.

## 2026-05-17 Personal-namespace launch adjustment

Shipped:

- Dropped the `plug-mcp` GitHub organization requirement for the public launch path.
- Kept the repository at the existing public `cyberpapiii/plug` remote and kept private vulnerability reporting enabled there.
- Updated workspace package metadata from `https://github.com/plug-mcp/plug` to `https://github.com/cyberpapiii/plug`.
- Updated Plug's MCP server-card identity and initialize metadata URLs to `io.github.cyberpapiii/plug` and `https://github.com/cyberpapiii/plug`.
- Updated cargo-dist Homebrew tap config to the existing public `cyberpapiii/homebrew-tap` repository.
- Updated README, operator guide, security policy, and user-story docs to use `cyberpapiii/plug`, `cyberpapiii/tap/plug`, and the git-based Cargo install path until crates.io publish is complete.
- Started tracking `Cargo.lock` so `cargo install --git https://github.com/cyberpapiii/plug plug-mcp --locked` is reproducible and does not fall back to an unlocked Git install.

Still blocked:

- crates.io publish still needs a token. The package name remains `plug-mcp`; publish order remains `plug-core` first, then `plug-mcp`.

## 2026-05-17 crates.io publish completion

Shipped:

- Created and saved a crates.io publish token locally with `cargo login`.
- Added and verified `robdezendorf@gmail.com` on the crates.io account.
- Published `plug-core 0.1.0` to crates.io.
- Published `plug-mcp 0.1.0` to crates.io after `plug-core` became available.
- Verified the public install path with `cargo install plug-mcp --locked --root /tmp/plug-crates-install --force`.
- Verified the installed binary with `/tmp/plug-crates-install/bin/plug --version`, which returned `plug 0.1.0`.
- Updated README, user stories, operator guide, and the audit distribution row so crates.io is now the primary Cargo install path and GitHub install is the unreleased-main fallback.

Remaining:

- GitHub release artifacts and the Homebrew tap still need a release cut through cargo-dist.
- The pre-existing unrelated `.letta/claude/*` deletions remain unstaged.

## 2026-05-17 local artifact cleanup

Shipped:

- Removed generated Cargo build output with `cargo clean`; Cargo reported `66.3GiB` removed.
- Removed stale temporary Plug install/audit directories under `/tmp`.
- Added `scripts/clean-build-artifacts.sh`, a dry-run-by-default cleanup helper for repo `target/`, `/tmp/plug-*`, and optional runtime artifact cache cleanup.
- Documented the cleanup helper in `docs/OPERATOR-GUIDE.md` and `CONTRIBUTING.md`.

Kept:

- `~/Library/Application Support/plug` was left intact because it contains live config, OAuth tokens, sockets, PID files, and operator tokens.
- `~/Library/Caches/plug/artifacts` was left intact by default because it stores recent `plug://artifact/...` result files; the new cleanup script can remove it explicitly with `--runtime-cache`.

## 2026-05-17 local reinstall cleanup integration

Shipped:

- Added `--clean` to `scripts/dev-reinstall.sh`.
- `./scripts/dev-reinstall.sh --quick --clean` now reinstalls the local binary, smoke-tests it, then runs `scripts/clean-build-artifacts.sh --yes`.
- Documented the flag in README, CONTRIBUTING, and the operator guide.

Decision:

- Cleanup remains opt-in for normal development so repeated compile/test loops do not rebuild the world every time.
- Release cleanup remains explicit because the repo currently uses direct `dist` commands rather than a single release wrapper script.

## 2026-05-17 current-truth doc reconciliation

Shipped:

- Reconciled stale `docs/PROJECT-STATE-SNAPSHOT.md` and `docs/PLAN.md` wording that still described daemon IPC resource subscribe as unsupported.
- Current-truth docs now match the hardened implementation and audit row: daemon IPC supports resource subscribe/unsubscribe and targeted resource update delivery.

## 2026-05-17 v0.3.0 release prep

Shipped:

- Bumped the workspace and publishable crate versions from `0.1.0` to `0.3.0` for the public artifact release.
- Updated `CHANGELOG.md` for the hardening/public-launch release.

Decision:

- Skipped `0.1.1` because the repository already has a historical `v0.2.0` tag. `0.3.0` gives the public launch a forward-moving tag and avoids making a newer release look older than an existing tag.
