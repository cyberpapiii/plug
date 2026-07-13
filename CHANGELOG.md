# Changelog

All notable changes to plug are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/)
and the project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

Detailed notes: [RMCP 2.2 upgrade](docs/RELEASE-NOTES-2026-07-13-RMCP-2.2-codex-5.6-sol.md) and [July 2026 reliability update](docs/RELEASE-NOTES-2026-07-12-codex-5.6-sol.md).

### Added

- End-to-end config watcher coverage for normal saves, atomic-renames, parse failures, and unrelated file changes.
- IPC proxy characterization coverage for reconnects, retries, malformed frames, notification ordering, and replayed session state.
- CI checks for the declared Rust 1.88 minimum version, RustSec advisories, and todo-file status consistency.

### Changed

- Reconnecting daemon clients now restore capabilities, resource subscriptions, client log level, and other session state before resuming work.
- Catalog refresh fetches resources, templates, and prompts concurrently and avoids repeated server lookups and unnecessary filtered views.
- Oversized artifact writes run on the blocking pool instead of occupying an async runtime worker.
- Native task creation and task teardown now use bounded waits derived from each upstream server's call timeout.
- Split the daemon implementation into focused framing, path, registry, auth, notification, and MCP dispatch modules without changing its public behavior.
- Source builds now require Rust 1.88.
- Upgraded the Rust MCP SDK from RMCP 1.7.0 to exactly RMCP 2.2.0 while preserving MCP `2025-11-25` negotiation and the existing transport/method surface.
- Migrated to RMCP's spec-aligned content, resource, prompt, task, elicitation, and cancellation APIs.
- Refreshed every direct Rust dependency to its latest compatible stable release, including Keyring 4.1.4, Rand 0.10.2, TOML 1.1.2, and Tower HTTP 0.7.0.

### Fixed

- Resource subscriptions now serialize upstream transitions per URI, preserve the correct recorded owner, and heal route changes without false success or zombie registry entries.
- HTTP and IPC session teardown now aborts local task execution and forwards bounded cancellation to task-capable upstreams.
- Task creation can no longer recreate records after the owning session has been removed, leak owner guards behind a full request queue, or lose cancellation in the send-to-record window.
- Reloads and reconnects now commit through the same coordination lock, so stale reconnect attempts cannot overwrite newer configuration.
- SSE replay preserves the unsent tail after a delivery failure and no longer clears a sender installed by a racing reconnect.
- Daemon IPC read silence now forces a reconnect instead of holding the session mutex indefinitely.
- Replacement grace tasks now participate in shutdown and a shutdown signal remains latched even when no receiver is present.
- Fixed expired-session counter underflow, pending cancellation replay, a daemon reverse-request busy loop, and closed-channel restoration after deregistration.
- Cancellation notifications without `requestId` are accepted and ignored safely instead of being mapped onto an unrelated active call.
- Downstream stdio and daemon-IPC initialization reject RMCP's announced-but-unimplemented MCP `2026-07-28` revision instead of accidentally negotiating it.
- Pinned `sse-stream` 0.2.4 to match the API required by RMCP 2.2.0 and keep fresh locked builds reproducible.
- Preserved complete TOML document parsing after the TOML 1.x upgrade for client discovery, imports, and doctor checks.
- Made Plug's documented 4 MiB HTTP request limit authoritative instead of Axum's hidden 2 MiB default.
- Local macOS reinstalls now sign and verify a staged binary before atomically replacing the live executable, eliminating the unsigned execution window that could retrigger Keychain prompts.
- Daemon auth-status queries no longer fall back to a missing token mirror's Keychain entry, preventing a read-only diagnostic from freezing IPC and HTTP behind a macOS authorization dialog.
- Engine concurrency tests now launch the prebuilt mock server directly, avoiding parallel `cargo run` lock contention that could exhaust their startup timeout on macOS CI.

### Security

- OAuth secret directories are created with owner-only permissions.
- Downstream OAuth state persistence fails closed on unsafe temporary-file permissions and enforces owner-only permissions after rename.
- Expired OAuth records are swept, equivalent scope sets reuse tokens, and client-credentials requests reuse live tokens instead of growing the store on every call.
- Replaced the unmaintained `fs2` lock dependency with `fs4` and removed the duplicate default HTTP stack from `oauth2`.

## [0.3.0] - 2026-05-17

### Added

- SSE reconnect replay for downstream Streamable HTTP sessions.
- Daemon IPC resource subscribe/unsubscribe and targeted resource update delivery.
- Operator source/trust metadata and clearer upstream-vs-inferred tool risk annotations.
- Trace correlation across downstream requests, router calls, retries, reconnects, and upstream HTTP proxying.
- SEP-2243 `Mcp-Method` / `Mcp-Name` validation and upstream header emission.
- Current server-card discovery at `/.well-known/mcp-server-card` with the legacy `/.well-known/mcp.json` alias preserved.
- RFC 9728 protected-resource metadata and client-credentials downstream OAuth support.
- Optional macOS stdio upstream sandboxing.
- Public crates.io packages under `plug-core` and `plug-mcp`.
- Build artifact cleanup helpers for local release and reinstall workflows.

### Changed

- Upgraded `rmcp` to `1.7.0`.
- Replaced the deprecated `serde_yml` parser with `serde_norway`.
- Updated public distribution metadata to the `cyberpapiii/plug` repository and `cyberpapiii/homebrew-tap`.
- Made `cargo install plug-mcp --locked` the primary public Cargo install path.

### Fixed

- Removed obsolete protocol-version response rewrite internals while preserving remote-client compatibility.
- Hardened OAuth discovery/challenge behavior and refresh-token handling.
- Kept daemon, HTTP, and stdio capability surfaces aligned after the hardening pass.

## [0.1.0] - 2026-03-04

### Features

- **core**: MCP multiplexer — shared upstream sessions, 4-tier tool routing
- **transport**: stdio transport for Claude Code, Cursor, Codex, Gemini CLI, and all MCP clients
- **transport**: streamable-HTTP + SSE transport with session management
- **transport**: DNS-rebinding prevention via Origin header validation
- **routing**: prefix-based tool routing (`servername__toolname` convention)
- **routing**: client-aware tool filtering (Cursor ≤40, Windsurf ≤100, VS Code ≤128)
- **routing**: fan-out tool calls with merge and conflict resolution
- **resilience**: circuit breaker per upstream server with half-open recovery
- **resilience**: exponential backoff with jitter on transient failures
- **resilience**: health checks with configurable intervals
- **config**: TOML configuration with layered overrides (file → env → CLI)
- **tui**: real-time Ratatui dashboard with server health, tool counts, event log
- **daemon**: headless daemon mode with PID file and lock management
- **http**: `GET /.well-known/mcp.json` server discovery card endpoint
- **cli**: `plug connect`, `plug status`, `plug tui` commands
- **dist**: single binary, zero runtime dependencies

[Unreleased]: https://github.com/cyberpapiii/plug/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/cyberpapiii/plug/releases/tag/v0.3.0
[0.1.0]: https://github.com/cyberpapiii/plug/releases/tag/v0.1.0
