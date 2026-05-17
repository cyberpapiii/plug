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
