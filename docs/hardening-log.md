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
