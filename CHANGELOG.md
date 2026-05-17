# Changelog

All notable changes to plug are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/)
and the project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
