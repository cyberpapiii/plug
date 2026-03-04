# Changelog

All notable changes to plug are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/)
and the project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

[Unreleased]: https://github.com/plug-mcp/plug/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/plug-mcp/plug/releases/tag/v0.1.0
