# Crate Stack

Every dependency decision with rationale. No dependency is included without a clear reason.

---

## Decision Framework

Before adding a crate:
1. Is this a solved problem? (Don't reinvent well-maintained wheels)
2. Is the crate actively maintained? (Last commit < 6 months)
3. Is it widely used? (Downloads, dependents, issues-to-stars ratio)
4. Does it compile to our targets? (macOS ARM/Intel, Linux x86/ARM/musl, Windows)
5. What's the binary size impact?
6. Does it have unsafe code? (Prefer pure-safe crates for application logic)

---

## Core Framework

### `rmcp` — Official MCP Rust SDK
- **Version**: 0.16.0
- **Why**: Official SDK from the MCP spec maintainers. Both client AND server handler traits. Streamable HTTP + stdio transports built-in. tokio-native. Task support (SEP-1686).
- **Features needed**: `server`, `client`, `macros`, `schemars`, `transport-io`, `transport-child-process`, `transport-streamable-http-client`, `transport-streamable-http-server`, `transport-streamable-http-client-reqwest`, `reqwest`
- **Risk**: Breaking API changes between versions (0.12 → 0.16 had migrations). Spec version may lag.
- **Alternative considered**: `rust-mcp-sdk` (v0.8.0) — more spec-current but less community adoption. `mcpkit` — nice macros but newer.
- **Research needed**: Verify rmcp supports 2025-11-25 fully. Verify both client+server can coexist in one binary. Test Streamable HTTP transport for both inbound and outbound proxy scenarios.

### `tokio` — Async Runtime
- **Version**: 1.49+
- **Why**: The standard async runtime for Rust. Multi-threaded with work-stealing scheduler. Required by rmcp, axum, and most networking crates.
- **Features needed**: `full` (includes rt-multi-thread, net, io, time, sync, macros, signal)
- **No alternative**: tokio is the only production-grade multi-threaded async runtime. async-std is less maintained.

### `axum` — HTTP Framework
- **Version**: 0.8+
- **Why**: Tower-native. First-class extractors (Host header for .localhost routing). Lightweight. Maintained by tokio team.
- **Features needed**: Default + `ws` (WebSocket for future use)
- **Alternative considered**: `actix-web` — more features but less Tower integration. `warp` — less actively maintained.
- **Research needed**: Confirm axum 0.8 API for Host-based routing. Test SSE streaming for legacy client support.

### `tower` — Service Middleware
- **Version**: 0.5+
- **Why**: Composable middleware layers (timeout, retry, rate-limit). Used by axum internally. Standard Rust middleware pattern.
- **Features needed**: `timeout`, `retry`, `limit`, `util`

### `hyper` — HTTP Protocol
- **Version**: 1.x
- **Why**: HTTP/1.1 + HTTP/2 implementation. Used by axum and reqwest internally. Not used directly — included transitively.

### `serde` + `serde_json` — Serialization
- **Version**: serde 1.x, serde_json 1.x
- **Why**: JSON-RPC messages. Config parsing. Tool definitions. Universal in Rust.

---

## TUI + CLI

### `ratatui` — Terminal UI Framework
- **Version**: 0.30.0
- **Why**: The standard Rust TUI framework. 13 built-in widgets. Diff-based rendering (sub-ms). Layout system with centering helpers. Modular crate split (core, widgets, crossterm backend). 11K stars.
- **Features needed**: Default (includes `crossterm` backend)
- **Alternative considered**: Charm Bubble Tea v2 (Go) — would require switching to Go. Bubble Tea v2 is still beta. Ratatui is stable and has full feature parity.
- **Research needed**: Test ratatui 0.30 layout ergonomics for our dashboard design. Evaluate `tachyonfx` for subtle transitions (connection status changes).

### `crossterm` — Terminal Backend
- **Version**: Latest stable
- **Why**: Cross-platform (macOS, Linux, Windows). Async event stream for tokio. Enhanced keyboard support (Kitty protocol). The recommended backend for ratatui.
- **Alternative considered**: `termion` (Unix only), `termwiz` (Wezterm-specific).

### `clap` — CLI Argument Parsing
- **Version**: 4.5+
- **Why**: The standard. Derive macro for clean declarative CLI definition. Subcommand support. Shell completions. Color help output.
- **Usage**: Derive pattern (declarative, clean):
  ```rust
  #[derive(Parser)]
  #[command(name = "fanout")]
  struct Cli { ... }
  ```

### `figment` — Layered Configuration
- **Version**: 0.10+
- **Why**: Composable config from multiple sources (TOML file + env vars + CLI overrides + defaults). Type-safe extraction via serde. The best config crate for layered configs.
- **Providers needed**: `Toml`, `Env`, `Serialized` (for defaults and CLI overrides)
- **Alternative considered**: `config` crate — less ergonomic. `clap_conf` — too tightly coupled to clap.
- **Research needed**: Test Figment's env var handling with `$VAR_NAME` references in TOML values.

### `directories` — XDG Paths
- **Version**: Latest
- **Why**: XDG-compliant config/data/state paths across platforms. `~/.config/fanout/`, `~/.local/share/fanout/`, etc.

---

## Proxy + Networking

### `reqwest` — HTTP Client
- **Version**: 0.12+
- **Why**: High-level HTTP client for upstream Streamable HTTP connections. rustls support. Streaming response body. Cookie jar. Proxy support.
- **Features needed**: `rustls-tls`, `json`, `stream`
- **Alternative considered**: `hyper` client directly — lower level, more control, more boilerplate.

### `rustls` — TLS
- **Version**: 0.23+
- **Why**: Pure-Rust TLS implementation. No OpenSSL dependency (critical for static/musl builds). FIPS-ready. Audited.
- **Used by**: reqwest and axum-server for TLS.
- **Research needed**: Test self-signed cert generation and trust for .localhost HTTPS.

### `rcgen` — Certificate Generation
- **Version**: 0.13+
- **Why**: Generate self-signed X.509 certificates with custom SANs for `.localhost` subdomains. Pure Rust.
- **When needed**: Only if HTTPS mode is enabled for .localhost routing.

---

## Resilience

### `tower-resilience` — Circuit Breaker + Resilience (UPDATED: replaces tower-circuitbreaker)
- **Version**: 0.7
- **Why**: Bundles circuit breaker, bulkhead, retry, rate limiting, and 10 other resilience patterns. The `tower-circuitbreaker` crate was deprecated by its author in favor of this workspace.
- **Configuration**: 50% failure rate → open, 30s cooldown, 2 probe calls in half-open.
- **Source**: https://github.com/joshrotenberg/tower-resilience (75 stars)

### `tokio::sync::Semaphore` — Concurrency Limiting (UPDATED: replaces flow-guard)
- **Why**: TCP Vegas (flow-guard) is overkill for <20 upstream servers. MCP server latency is inherently variable (LLM inference, tool execution), so Vegas would misinterpret normal variance as congestion. A simple semaphore with configurable per-server limits is the right tool.
- **No additional crate needed** — built into tokio.

### `backon` — Exponential Backoff (UPDATED: replaces backoff)
- **Version**: 1.6.0
- **Why**: The `backoff` crate is unmaintained. `backon` is actively maintained, supports async/blocking/wasm/no_std, and has a stable 1.0+ API.
- **Source**: https://crates.io/crates/backon

---

## Concurrency + State

### `dashmap` — Concurrent HashMap
- **Version**: 6.x
- **Why**: Lock-free concurrent HashMap for hot paths (tool cache, session registry, request mapping). Sharded internally for low contention.
- **Alternative considered**: `std::sync::RwLock<HashMap>` — works but higher contention on write-heavy paths.

### `arc-swap` — Lock-Free Config Swap
- **Version**: 1.x
- **Why**: Atomic pointer swap for hot-reloading config. Readers get a snapshot with zero lock contention. Perfect for config that's read on every request but written rarely.
- **Used for**: `ArcSwap<Config>` — config hot-reload without locks.

### `notify` — Filesystem Watcher
- **Version**: Latest
- **Why**: Watch config file for changes. Cross-platform (inotify on Linux, FSEvents on macOS, ReadDirectoryChanges on Windows).
- **Used for**: Config hot-reload trigger.

---

## Observability

### `tracing` + `tracing-subscriber` — Structured Logging
- **Version**: tracing 0.1+, tracing-subscriber 0.3+
- **Why**: Structured, span-based instrumentation. Async-aware (spans survive across await points). Filterable by level and module. The Rust standard for observability.

### `tracing-appender` — File Logging
- **Version**: 0.2+
- **Why**: Non-blocking file writer with daily rotation. For daemon/headless mode where stdout is not available.

### Custom TUI Log Widget (UPDATED: replaces tui-logger)
- **Why**: `tui-logger` 0.17.x pins `ratatui = "0.29"`, conflicting with our target of ratatui 0.30.0. Rather than pin an older ratatui, we'll build a minimal custom widget (~100-200 LOC) that subscribes to tracing events directly. Ratatui's built-in tracing recipe provides a reference implementation.
- **Source**: https://ratatui.rs/recipes/apps/log-with-tracing/

### `metrics` — Application Metrics (Optional, Phase 5)
- **Version**: 0.24+
- **Why**: Counters, histograms, gauges for tool call latency, error rates, connection counts. Optional Prometheus exporter.
- **Deferred**: Not needed for MVP. Add when observability becomes a priority.

---

## Error Handling

### `thiserror` — Error Derive
- **Version**: 2.x
- **Why**: Clean `#[derive(Error)]` for library/domain error types. Zero runtime overhead.

### `anyhow` — Application Errors
- **Version**: 1.x
- **Why**: Flexible error handling for application code where exact types don't matter. Context chaining with `.context("doing X")`.
- **Rule**: Use `thiserror` for domain types that cross API boundaries. Use `anyhow` for internal application errors.

---

## Distribution

### `cargo-dist` — Release Pipeline
- **Version**: 0.30+
- **Why**: Generates GitHub Actions CI for cross-platform binary releases. Creates Homebrew formulae. Shell/PowerShell install scripts.
- **Targets**: aarch64-apple-darwin, x86_64-apple-darwin, x86_64-unknown-linux-gnu, x86_64-unknown-linux-musl, aarch64-unknown-linux-gnu, aarch64-unknown-linux-musl, x86_64-pc-windows-msvc

### Build Profile

```toml
[profile.release]
strip = true        # Remove debug symbols (~80% size reduction)
lto = true          # Link-time optimization (10-20% size reduction)
codegen-units = 1   # Better optimization (slower compile)
opt-level = "s"     # Optimize for size (good balance)
panic = "abort"     # No unwind tables (~10% size reduction)
```

Target binary size: < 10 MB (release, stripped).

---

## Crates NOT Included (And Why)

| Crate | Why Not |
|-------|---------|
| `sqlx` / `sqlite` | No database. Config is TOML, state is in-memory. |
| `openssl` | Using rustls (pure Rust, no system deps). |
| `actix-web` | Using axum (Tower-native, maintained by tokio team). |
| `sled` / `redb` | No persistent storage needed. |
| `tonic` | No gRPC. MCP is JSON-RPC over HTTP. |
| `redis` | No external state store. Single-node only. |
| `indicatif` | Using ratatui's built-in progress widgets instead. |
| `dialoguer` | No interactive prompts in the CLI (non-interactive by design). TUI handles interaction. |

---

## Resolved Research Questions (2026-03-03)

1. **rmcp proxy pattern**: YES — coexist cleanly. AgentGateway validates this. See `docs/research/rmcp-feasibility.md`.

2. **tower-circuitbreaker maturity**: DEPRECATED — replaced by `tower-resilience` v0.7.

3. **flow-guard vs simple semaphore**: Semaphore wins. TCP Vegas overkill for <20 servers.

4. **Figment env var interpolation**: NO native support. Need custom post-processor (~50 LOC) to expand `$VAR_NAME` references in TOML values. The idiomatic Figment approach is to use the `Env` provider as an overlay, but that doesn't support inline references.

5. **ratatui + tui-logger integration**: CONFLICT — tui-logger pins ratatui 0.29. Build custom widget instead.

6. **axum SSE server**: Use separate routes — `/mcp` for Streamable HTTP (POST/GET/DELETE), `/sse` for legacy SSE (GET → endpoint event). AgentGateway's `LegacySSEService` is the reference implementation.

7. **crossterm Mode 2026**: Deferred to Phase 4 TUI implementation.
