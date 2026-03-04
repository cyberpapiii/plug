# CLAUDE.md — plug

## What This Project Is

plug is a ruthlessly minimal MCP multiplexer written from scratch in Rust. It is a single binary that sits between all AI coding clients (Claude Code, Cursor, Gemini CLI, Codex, Windsurf, VS Code Copilot, OpenCode, Zed, etc.) and all MCP servers — multiplexing, routing, and serving tools with zero friction.

**Status**: Pre-development. The `docs/` folder contains the complete specification, architecture, and research. No code has been written yet.

## Documentation Map (Read These First)

| Document | What It Contains |
|----------|-----------------|
| `docs/VISION.md` | Non-negotiable design principles, anti-principles, quality bar |
| `docs/USERS.md` | 4 personas, 30+ user stories (with IDs like S1, M1, C1), 5 scenarios, 10 edge cases |
| `docs/ARCHITECTURE.md` | Component design, data flows for tool calls and fan-out, concurrency model, security model |
| `docs/MCP-SPEC.md` | MCP 2025-11-25 wire format, transports, methods, upcoming changes |
| `docs/CLIENT-COMPAT.md` | Every AI client's quirks, hard limits, config formats, detection strategy |
| `docs/COMPETITIVE.md` | 25+ competitors analyzed, gap analysis, patterns to adopt |
| `docs/UX-DESIGN.md` | CLI commands, TUI layout, output modes, first-run flow, agent UX |
| `docs/CRATE-STACK.md` | Every dependency with rationale, alternatives considered, open research questions |
| `docs/PLAN.md` | 5-phase implementation plan with checkboxes |
| `docs/RESEARCH-BREADCRUMBS.md` | 29 open questions (Q1-Q5, E1-E29) that must be resolved |

## Tech Stack

- **Language**: Rust (2024 edition)
- **MCP SDK**: rmcp v0.16.0 (official Rust SDK — both client and server roles)
- **TUI**: Ratatui v0.30.0 + Crossterm
- **CLI**: Clap v4.5 (derive pattern)
- **HTTP**: Axum 0.8+ / Tower 0.5+ / Hyper 1.x
- **Async**: Tokio (multi-threaded with work-stealing)
- **Config**: TOML via Figment (layered: defaults → file → env → CLI)
- **Concurrency**: DashMap, ArcSwap, tokio::sync primitives
- **Resilience**: tower-circuitbreaker, backoff, flow-guard
- **Observability**: tracing + tracing-subscriber + tracing-appender
- **Distribution**: cargo-dist, Homebrew tap

## Core Principles (From VISION.md)

1. **Single binary, zero dependencies** — `brew install plug && plug`
2. **Ruthlessly minimal** — if a feature can't be explained in one sentence, simplify it
3. **Dual-audience UX** — every command works for humans (pretty) AND agents (`--output json`)
4. **Token-efficient** — 5-layer optimization, client-aware tool filtering
5. **Clean pass-through** — faithful proxy by default, optional enrichment
6. **Rock-solid reliable** — circuit breakers, merge cache, graceful degradation
7. **Future-proof** — MCP 2025-11-25, ready for stateless mode (June 2026)

## Things We Will NEVER Do

- Require Docker, a database, an account, or a cloud service
- Add enterprise features (RBAC, multi-tenancy, OIDC) at the cost of simplicity
- Break the pass-through contract unless the user explicitly opted in
- Log secrets (tokens, keys, credentials)
- Require sudo/admin (port 3282, above 1024)

## Key Architecture Decisions

- **Shared upstream sessions**: N clients share 1 connection per upstream server (not N connections)
- **4-tier tool routing**: cache → prefix → negative cache → fan-out
- **Client detection**: parse `clientInfo.name` from `InitializeRequest` to auto-apply tool limits (Cursor 40, Windsurf 100, VS Code 128)
- **Portless-native**: `servername.localhost:3282` routing via Host header (built natively, not using Vercel's Node.js Portless)
- **Daemon vs embedded**: Phase 1 uses embedded (each `plug connect` is independent). Phase 4 adds daemon mode for TUI.

## Development Commands

```bash
# Build
cargo build --release

# Run
cargo run

# Test
cargo test

# Check
cargo clippy -- -D warnings
cargo fmt --check

# Release profile (in Cargo.toml)
# strip = true, lto = true, codegen-units = 1, opt-level = "s", panic = "abort"
```

## Project Structure (Planned)

```
Cargo.toml                  # Workspace root
plug-core/                # Library crate — all business logic (UI-agnostic)
  src/
    config/                 # Config parsing, validation, hot-reload
    engine/                 # Core multiplexer engine
    transport/              # stdio, HTTP, SSE transports (inbound + outbound)
    session/                # Client and upstream session management
    routing/                # Tool routing, fan-out, merge
    resilience/             # Circuit breaker, health checks, backpressure
plug/                     # Binary crate — TUI, CLI, daemon
  src/
    main.rs
    cli/                    # Clap command definitions
    tui/                    # Ratatui dashboard
    daemon/                 # Headless mode
```
