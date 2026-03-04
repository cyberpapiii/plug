# Crate Dependency Validation Report

**Project:** plug (MCP multiplexer)
**Date:** 2026-03-03
**Validated by:** Automated research via web search + crates.io + GitHub API

---

## Summary Table

| # | Crate | Documented Version | Latest Verified | Status | Action Required |
|---|-------|--------------------|-----------------|--------|-----------------|
| 1 | rmcp | 0.16.0 | 0.16.0 | Current | None (see notes on rapid release cadence) |
| 2 | tokio | 1.49+ | 1.49.0 | Current | None |
| 3 | axum | 0.8+ | 0.8.8 | Current | None |
| 4 | tower | 0.5+ | 0.5.3 | Current | None |
| 5 | ratatui | 0.30.0 | 0.30.0 | Current | None |
| 6 | crossterm | -- | 0.29.0 | N/A | Use 0.29.0 |
| 7 | clap | 4.5+ | 4.5.60 | Current | None |
| 8 | figment | 0.10+ | 0.10.19 | Current | **Custom deserializer needed** for `$VAR_NAME` interpolation |
| 9 | dashmap | 6.x | 6.1.0 stable (7.0.0-rc2 pre-release) | Current | Stay on 6.1.0 for stability |
| 10 | arc-swap | 1.x | 1.7.1 | Current | None |
| 11 | reqwest | 0.12+ | 0.13.2 | **Update needed** | Upgrade to 0.13; review breaking changes |
| 12 | rustls | 0.23+ | 0.23.36 | Current | None |
| 13 | rcgen | 0.13+ | 0.14.6 | **Update available** | Upgrade to 0.14.6; wildcard SANs supported |
| 14 | serde | -- | 1.0.228 | Stable | None |
| 14 | serde_json | -- | 1.0.149 | Stable | None |
| 15 | thiserror | 2.x | 2.0.17 | Current | None |
| 16 | anyhow | 1.x | 1.0.98 | Current | None |
| 17 | tracing | -- | 0.1.44 | Stable | None |
| 17 | tracing-subscriber | -- | 0.3.20 | Stable | None |
| 17 | tracing-appender | -- | 0.2.3 | Stable | None |
| 18 | notify | -- | 8.2.0 | Stable | None |
| 19 | backoff | 0.4+ | 0.4.0 | **UNMAINTAINED** | **Replace with `backon` 1.6.0** |
| 20 | tower-circuitbreaker | -- | 0.1.0 | **DEPRECATED** | **Use `tower-resilience` instead** |
| 21 | flow-guard | -- | 0.1.0 | Pre-alpha | **Overkill; use `tokio::sync::Semaphore`** |
| 22 | tui-logger | -- | 0.17.3 | Active | Compatible with tracing-subscriber Layer; pin ratatui carefully |

---

## Detailed Findings

### 1. rmcp (0.16.0)

- **Latest version:** 0.16.0 (confirmed on [crates.io](https://crates.io/crates/rmcp))
- **Repository:** Now the official Rust MCP SDK at [modelcontextprotocol/rust-sdk](https://github.com/modelcontextprotocol/rust-sdk) (originally [4t145/rmcp](https://github.com/4t145/rmcp))
- **Release cadence:** Extremely active. 22+ versions released since March 2025, with versions 0.5.0 through 0.16.0 landing in rapid succession.
- **Breaking changes:** The project has a migration guide for 1.x in the repository. Given the pre-1.0 version, expect API churn between minor versions. Pin to exact version in `Cargo.toml` (e.g., `rmcp = "=0.16.0"`) for stability.
- **Recommendation:** Version 0.16.0 is current. Monitor closely as MCP protocol spec changes drive API updates. Use `rmcp = { version = "0.16", features = ["server"] }`.

**Source:** [crates.io/crates/rmcp](https://crates.io/crates/rmcp), [docs.rs/rmcp](https://docs.rs/crate/rmcp/latest), [GitHub](https://github.com/modelcontextprotocol/rust-sdk)

### 2. tokio (1.49.0)

- **Latest version:** 1.49.0 (released 2026-01-03)
- **LTS releases:**
  - 1.43.x -- LTS until March 2026 (MSRV 1.70)
  - 1.47.x -- LTS until September 2026 (MSRV 1.70)
- **Release cadence:** ~1 minor release per month
- **Recommendation:** Use `tokio = "1.49"` with `features = ["full"]`. For maximum stability, consider the 1.47.x LTS line.

**Source:** [crates.io/crates/tokio](https://crates.io/crates/tokio), [docs.rs/tokio](https://docs.rs/crate/tokio/latest)

### 3. axum (0.8.8)

- **Latest version:** 0.8.8
- **Host-based routing:** axum does NOT natively support hostname-based routing. The recommended pattern is to use a middleware or extractor that reads the `Host` header and dispatches to different `Router` instances. See [kerkour.com/rust-axum-hostname-router](https://kerkour.com/rust-axum-hostname-router) for a full example and [GitHub Discussion #934](https://github.com/tokio-rs/axum/discussions/934).
- **Recommendation:** Use `axum = "0.8"`. For the plug multiplexer's virtual-host routing (e.g., `server-name.localhost`), implement a custom middleware layer that extracts the `Host` header and routes to the appropriate upstream MCP server.

**Source:** [crates.io/crates/axum](https://crates.io/crates/axum), [docs.rs/axum](https://docs.rs/axum/latest/axum/)

### 4. tower (0.5.3)

- **Latest version:** 0.5.3
- **MSRV:** 1.64.0
- **Recommendation:** Use `tower = "0.5"`. Stable and well-maintained.

**Source:** [crates.io/crates/tower](https://crates.io/crates/tower), [docs.rs/tower](https://docs.rs/crate/tower/latest)

### 5. ratatui (0.30.0)

- **Latest version:** 0.30.0 (described as "the biggest release of ratatui so far")
- **Key new features for dashboard layouts:**
  - `no_std` support for embedded targets
  - Modularized workspace architecture (ratatui-core split out)
  - Major widget & layout upgrades
  - Stacked charts; text-over-block in Canvas
  - Feature flags for crossterm version selection (`crossterm_0_28`, `crossterm_0_29`)
- **Downloads:** 11.9M+ total
- **Recommendation:** Use `ratatui = "0.30"`. The layout improvements are ideal for the plug dashboard. Use `crossterm_0_29` feature flag.

**Source:** [crates.io/crates/ratatui](https://crates.io/crates/ratatui/), [GitHub releases](https://github.com/ratatui/ratatui/releases), [ratatui.rs](https://ratatui.rs/)

### 6. crossterm (0.29.0)

- **Latest version:** 0.29.0
- **Recommendation:** Use `crossterm = "0.29"`. Ensure ratatui feature flag `crossterm_0_29` is enabled for compatibility.

**Source:** [crates.io/crates/crossterm](https://crates.io/crates/crossterm), [docs.rs/crossterm](https://docs.rs/crate/crossterm/latest)

### 7. clap (4.5.60)

- **Latest version:** 4.5.60
- **Actively maintained** with regular patch releases.
- **Recommendation:** Use `clap = { version = "4.5", features = ["derive"] }`.

**Source:** [crates.io/crates/clap](https://crates.io/crates/clap), [docs.rs/clap](https://docs.rs/crate/clap/latest)

### 8. figment (0.10.19) -- CRITICAL FINDING

- **Latest version:** 0.10.19
- **`$VAR_NAME` interpolation in TOML: NOT SUPPORTED NATIVELY.**
  - Figment's `Env` provider reads environment variables as standalone config sources (e.g., `APP_SERVER_PORT=8080` maps to `server.port`).
  - Figment's `Toml` provider parses TOML files as-is; it does NOT perform string interpolation or variable substitution within TOML values.
  - If you write `api_key = "$OPENAI_API_KEY"` in a TOML file, figment will read the literal string `"$OPENAI_API_KEY"`, NOT the environment variable value.
  - TOML itself explicitly ruled out variable interpolation as a feature.

- **Solutions (in order of preference):**
  1. **Custom post-processing deserializer:** After figment merges all sources, walk the resulting `Value` tree and replace any string matching `$VAR_NAME` or `${VAR_NAME}` with `std::env::var("VAR_NAME")`. This is ~50 lines of code.
  2. **Template pre-processing:** Read the TOML file as a string, perform regex substitution on `$VAR_NAME` patterns, then feed the result to `figment::providers::Toml::string()`.
  3. **Use figment's `Env` provider as an overlay:** Instead of `$VAR_NAME` in TOML, let users set `PLUG_SERVER_API_KEY=sk-xxx` and have the `Env` provider override the TOML value. This is the idiomatic figment approach.
  4. **Alternative libraries:** [noml](https://github.com/noml-lang/noml-rust) supports variable interpolation natively, but is not TOML-compatible.

- **Recommendation:** Use approach #3 (Env overlay) as the primary mechanism, with approach #1 (custom post-processor) for users who strongly prefer `$VAR` syntax in config files.

**Source:** [docs.rs/figment](https://docs.rs/figment/latest/figment/), [GitHub](https://github.com/SergioBenitez/Figment), [Env provider docs](https://docs.rs/figment/latest/figment/providers/struct.Env.html)

### 9. dashmap (6.1.0)

- **Latest stable:** 6.1.0
- **Pre-release:** 7.0.0-rc2 (released ~March 2025, still RC after 12 months -- may be stalled)
- **Downloads:** 173.7M+ total
- **Recommendation:** Use `dashmap = "6.1"`. Do not use the 7.0.0-rc2 pre-release in production.

**Source:** [crates.io/crates/dashmap](https://crates.io/crates/dashmap), [docs.rs/dashmap](https://docs.rs/crate/dashmap/latest)

### 10. arc-swap (1.7.1)

- **Latest version:** 1.7.1
- **Recommendation:** Use `arc-swap = "1.7"`. Mature and stable.

**Source:** [crates.io/crates/arc-swap](https://crates.io/crates/arc-swap), [docs.rs/arc-swap](https://docs.rs/crate/arc-swap/latest)

### 11. reqwest (0.13.2) -- VERSION UPDATE NEEDED

- **Latest version:** 0.13.2 (documented as 0.12+, now 0.13.x)
- **Breaking changes from 0.12 to 0.13:**
  - `query()` method on `RequestBuilder` now requires an explicit feature flag
  - Default TLS backend changed to rustls (previously native-tls)
  - Redirect policy types renamed (e.g., `reqwest::RedirectPolicy` -> `reqwest::redirect::Policy`)
  - Uses `rustls-platform-verifier` instead of `rustls-native-certs`
- **Downloads:** 381M+ total
- **Recommendation:** Upgrade to `reqwest = { version = "0.13", features = ["json", "rustls-tls"] }`. Review the [CHANGELOG](https://github.com/seanmonstar/reqwest/blob/master/CHANGELOG.md) for full migration details.

**Source:** [crates.io/crates/reqwest](https://crates.io/crates/reqwest), [docs.rs/reqwest](https://docs.rs/crate/reqwest/latest), [GitHub releases](https://github.com/seanmonstar/reqwest/releases)

### 12. rustls (0.23.36)

- **Latest version:** 0.23.36
- **Recommendation:** Use `rustls = "0.23"`. This is the current stable series.

**Source:** [crates.io/crates/rustls](https://crates.io/crates/rustls), [docs.rs/rustls](https://docs.rs/crate/rustls/latest)

### 13. rcgen (0.14.6) -- Wildcard SAN Support Confirmed

- **Latest version:** 0.14.6 (released 2025-12-13; documented as 0.13+)
- **Wildcard SAN for `*.localhost`:** YES, supported.
  - Use `SanType::DnsName(Ia5String)` with the value `"*.localhost"`
  - Add to `CertificateParams.subject_alt_names` vector
  - Example:
    ```rust
    let mut params = CertificateParams::default();
    params.subject_alt_names = vec![
        SanType::DnsName("*.localhost".try_into().unwrap()),
        SanType::DnsName("localhost".try_into().unwrap()),
    ];
    ```
- **Recommendation:** Upgrade to `rcgen = "0.14"`. The 0.13->0.14 upgrade includes API improvements; review the [CHANGELOG](https://docs.rs/crate/rcgen/latest/source/CHANGELOG.md).

**Source:** [crates.io/crates/rcgen](https://crates.io/crates/rcgen), [SanType docs](https://docs.rs/rcgen/latest/rcgen/enum.SanType.html), [GitHub](https://github.com/rustls/rcgen)

### 14. serde / serde_json

- **serde:** 1.0.228 (stable, ubiquitous)
- **serde_json:** 1.0.149 (stable)
- **Recommendation:** Use `serde = { version = "1.0", features = ["derive"] }` and `serde_json = "1.0"`.

**Source:** [crates.io/crates/serde](https://crates.io/crates/serde), [crates.io/crates/serde_json](https://crates.io/crates/serde_json)

### 15. thiserror (2.0.17)

- **Latest version:** 2.0.17
- **Recommendation:** Use `thiserror = "2.0"`. The 2.x series is the current stable line.

**Source:** [crates.io/crates/thiserror](https://crates.io/crates/thiserror), [docs.rs/thiserror](https://docs.rs/crate/thiserror/latest)

### 16. anyhow (1.0.98)

- **Latest version:** 1.0.98
- **Recommendation:** Use `anyhow = "1.0"`. Maintained by David Tolnay; very stable.

**Source:** [crates.io/crates/anyhow](https://crates.io/crates/anyhow)

### 17. tracing ecosystem

| Crate | Latest | Notes |
|-------|--------|-------|
| tracing | 0.1.44 | Stable; MSRV 1.42 |
| tracing-subscriber | 0.3.20 | Stable; MSRV 1.65 |
| tracing-appender | 0.2.3 | Stable; MSRV 1.63 |

- **Recommendation:** Use `tracing = "0.1"`, `tracing-subscriber = { version = "0.3", features = ["env-filter", "fmt", "json"] }`, `tracing-appender = "0.2"`.

**Source:** [crates.io/crates/tracing](https://crates.io/crates/tracing), [crates.io/crates/tracing-subscriber](https://crates.io/crates/tracing-subscriber), [crates.io/crates/tracing-appender](https://crates.io/crates/tracing-appender)

### 18. notify (8.2.0)

- **Latest version:** 8.2.0
- **MSRV:** 1.77
- **Platform support:** Linux (inotify), macOS (FSEvents/kqueue), Windows (ReadDirectoryChangesW), BSDs (kqueue), fallback polling
- **Used by:** alacritty, cargo-watch, deno, rust-analyzer, watchexec, mdBook
- **Recommendation:** Use `notify = "8.2"`. Battle-tested and well-maintained.

**Source:** [crates.io/crates/notify](https://crates.io/crates/notify), [docs.rs/notify](https://docs.rs/crate/notify/latest)

### 19. backoff (0.4.0) -- UNMAINTAINED, REPLACE

- **Latest version:** 0.4.0
- **Status:** **UNMAINTAINED.** The original maintainer is no longer pushing updates, fixing bugs, or addressing security concerns.
- **Replacement:** **[backon](https://crates.io/crates/backon) v1.6.0**
  - Actively maintained by [Xuanwo](https://github.com/Xuanwo/backon)
  - API: `your_fn.retry(ExponentialBuilder::default()).await`
  - Supports async + blocking + wasm + no_std
  - Drop-in replacement with similar exponential/constant backoff strategies
  - 1.0+ stable API

- **Recommendation:** Remove `backoff` dependency. Replace with `backon = "1.6"`.

**Source:** [crates.io/crates/backoff](https://crates.io/crates/backoff), [crates.io/crates/backon](https://crates.io/crates/backon), [Why backoff is unmaintained](https://magazine.ediary.site/blog/rusts-backoff-crate-why-its), [backon design](https://rustmagazine.org/issue-2/how-i-designed-the-api-for-backon-a-user-friendly-retry-crate/)

---

## Specific Crate Investigations

### 20. tower-circuitbreaker -- DEPRECATED

- **Version:** 0.1.0
- **Status:** **DEPRECATED and no longer maintained.**
- **GitHub:** [joshrotenberg/tower-circuitbreaker](https://github.com/joshrotenberg/tower-circuitbreaker) -- 3 stars, 19 commits total
- **Downloads:** 535 all-time (very low adoption)
- **Last activity:** May 2025 (v0.1.0 release, then immediately deprecated)
- **Deprecation notice:** The README directs users to [tower-resilience](https://github.com/joshrotenberg/tower-resilience), which consolidates circuit breaker + bulkhead + rate limiter + retry + cache + fallback + hedge + health check into a single workspace.

**Replacement: tower-resilience**
- **GitHub:** [joshrotenberg/tower-resilience](https://github.com/joshrotenberg/tower-resilience) -- 75 stars
- **Version:** 0.7 on crates.io
- **Sub-crates:** circuit breaker, bulkhead, time limiter, retry, rate limiter, cache, fallback, hedge, reconnect, health check, executor, adaptive concurrency, coalesce, chaos
- **Maturity:** Pre-1.0 but actively maintained, inspired by Resilience4j patterns
- **Activity:** Commits visible through early 2026

**Other alternatives:**
- [failsafe-rs](https://github.com/dmexe/failsafe-rs) -- Circuit breaker with detection/encapsulation of recurring failures
- [circuitbreaker-rs](https://github.com/copyleftdev/circuitbreaker-rs) -- Production-grade, lock-efficient, observability-ready
- **DIY with tower middleware** -- For simple use cases, a custom Tower `Layer` wrapping state with `AtomicU64` counters may suffice

**Recommendation:** Use `tower-resilience` for the circuit breaker pattern. It provides a complete resilience toolkit aligned with Tower's service model. For plug's use case (protecting upstream MCP servers), the circuit breaker + retry + bulkhead combination is ideal.

### 21. flow-guard -- Overkill for <20 Servers

- **Version:** 0.1.0 (pre-alpha)
- **Status:** Early-stage, open-core project
- **Algorithm:** TCP Vegas congestion control adapted for application-level concurrency
- **What it does:** Monitors RTT (round-trip time) and dynamically adjusts concurrency limits. If latency rises, it reduces concurrency; if the system is fast, it expands capacity.
- **Enterprise features (planned):** Distributed flow control via Redis/NATS

**Analysis for plug's use case (<20 upstream MCP servers):**

TCP Vegas-style adaptive concurrency is designed for large-scale distributed systems where hundreds or thousands of backends experience unpredictable load. For plug's scenario:

- **Server count:** <20 upstream MCP servers (typically 3-10 for most users)
- **Traffic pattern:** Low-to-moderate RPS per server; MCP requests are often long-lived (tool calls, sampling)
- **Latency signal:** MCP server latency is highly variable by nature (LLM inference, tool execution) -- Vegas would interpret normal variance as congestion

**Verdict: TCP Vegas is overkill and potentially counterproductive.**

**Recommended alternative:** `tokio::sync::Semaphore` with fixed or configurable limits.

```rust
use tokio::sync::Semaphore;
use std::sync::Arc;

struct UpstreamServer {
    name: String,
    max_concurrent: usize,
    semaphore: Arc<Semaphore>,
}

impl UpstreamServer {
    fn new(name: String, max_concurrent: usize) -> Self {
        Self {
            name,
            max_concurrent,
            semaphore: Arc::new(Semaphore::new(max_concurrent)),
        }
    }

    async fn execute<F, T>(&self, f: F) -> Result<T, Error>
    where
        F: Future<Output = Result<T, Error>>,
    {
        let _permit = self.semaphore.acquire().await?;
        f.await
    }
}
```

This gives you:
- Per-server concurrency limits (configurable in plug.toml)
- Zero external dependencies
- Predictable behavior
- Easy to reason about and debug

If adaptive concurrency is later needed, `tower-resilience`'s adaptive-concurrency sub-crate is a better choice than flow-guard.

### 22. tui-logger -- Compatible with Caveats

- **Latest version:** 0.17.3 (0.17.4 visible in docs.rs features page)
- **GitHub:** [gin66/tui-logger](https://github.com/gin66/tui-logger) -- 300 stars
- **Last commit on master:** January 19, 2024 (over 2 years ago -- concerning, though published crate versions are newer)
- **Downloads:** Actively used in the ratatui ecosystem

**TuiTracingSubscriberLayer + tracing-subscriber:**
- YES, `TuiTracingSubscriberLayer` implements `tracing_subscriber::Layer` trait
- Enable with feature flag: `tui-logger = { version = "0.17", features = ["tracing-support"] }`
- Works with tracing-subscriber's layered subscriber pattern:
  ```rust
  use tracing_subscriber::prelude::*;
  tracing_subscriber::registry()
      .with(tui_logger::TuiTracingSubscriberLayer)
      .with(tracing_subscriber::fmt::layer())
      .init();
  ```

**Ratatui 0.30 compatibility:**
- tui-logger 0.17.3 pins `ratatui = "0.29"` as its dependency
- This means it is NOT yet compatible with ratatui 0.30.0 out of the box
- You may need to wait for a tui-logger update or use ratatui 0.29 for the dashboard component
- Alternative: Fork tui-logger temporarily and bump the ratatui dependency

**Performance considerations:**
- The main log buffer uses a locking scheme to avoid blocking log macros during widget updates
- A double-buffer approach is used: logs accumulate in one buffer while the widget renders from another
- For plug's dashboard (moderate log volume), this should be adequate
- High-throughput logging (>10K events/sec) could cause contention; consider filtering with `tracing_subscriber::EnvFilter` to reduce volume

**Recommendation:** Use tui-logger 0.17 with `tracing-support` feature. Pin ratatui to 0.29 if using tui-logger, OR implement a minimal custom TUI log widget on ratatui 0.30 directly. The ratatui official docs provide a [recipe for logging with tracing](https://ratatui.rs/recipes/apps/log-with-tracing/) that may be sufficient without tui-logger.

**Source:** [crates.io/crates/tui-logger](https://crates.io/crates/tui-logger), [GitHub](https://github.com/gin66/tui-logger), [docs.rs/tui-logger](https://docs.rs/tui-logger/latest/tui_logger/), [Ratatui tracing recipe](https://ratatui.rs/recipes/apps/log-with-tracing/)

---

## Name Availability: "plug"

### crates.io

- **Status:** No crate named exactly `"plug"` was found in search results. A direct visit to `crates.io/crates/plug` could not be rendered (crates.io requires JavaScript). The search returned related crates (`plugy`, `cln-plugin`, `nih-plug`) but not `plug` itself.
- **Assessment:** Likely **available** or reserved-but-empty. Verify by running `cargo search plug --limit 1` or checking crates.io directly.
- **Related names:** `plugy` (WASM plugin system) exists. Consider `plug-mcp` as a fallback name.

### Homebrew

- **Status:** **TAKEN.** A Homebrew cask named `plug` already exists.
  - [formulae.brew.sh/cask/plug](https://formulae.brew.sh/cask/plug)
  - Description: "Music player for The Hype Machine"
  - Version: 2.0.19,2067
  - Requires macOS >= 10.11
  - Install: `brew install --cask plug`
- **Note:** This is a *cask* (GUI app), not a *formula* (CLI tool). You could potentially publish a formula named `plug` since the namespace is separate, but this would create user confusion.
- **Recommendation:** Use a different Homebrew formula name: `plug-mcp`, `plugmcp`, or `mcp-plug`.

### GitHub

- **github.com/plug:** **TAKEN.** The `plug` organization exists.
  - Created: 2009-08-09
  - Website: http://plugfr.org/
  - Public repos: 5
  - Appears to be a French user group (PLUG = Provencal Linux User Group)

- **github.com/plug-mcp:** **AVAILABLE.** GitHub API returns 404 for this organization. It can be claimed.

- **Recommendation:** Use `github.com/plug-mcp` as the GitHub organization.

### Domain: plug.dev

- **Status:** **TAKEN.**
  - [plug.dev](https://plug.dev/) is an active website
  - Description: "The creator platform for DevTools" -- a marketplace connecting DevTool companies with content creators/influencers
  - Actively operated business
- **Recommendation:** Consider alternative domains:
  - `plugmcp.dev`
  - `getplug.dev`
  - `plug-mcp.dev`
  - `plug.rs`

### Name Availability Summary

| Platform | "plug" | "plug-mcp" |
|----------|--------|------------|
| crates.io | Likely available | Available |
| Homebrew formula | Cask taken (separate namespace) | Available |
| GitHub org | Taken (PLUG FR) | **Available** |
| Domain (.dev) | Taken (DevTool platform) | Unknown (check registrar) |

---

## Critical Action Items

1. **REPLACE `backoff` with `backon`** -- The backoff crate is unmaintained. Switch to `backon = "1.6"` immediately.

2. **REPLACE `tower-circuitbreaker`** -- Deprecated. Use `tower-resilience` (circuit-breaker sub-crate) instead.

3. **DROP `flow-guard`** -- Use `tokio::sync::Semaphore` for concurrency limiting. TCP Vegas is inappropriate for <20 upstream servers with variable-latency MCP calls.

4. **UPGRADE `reqwest` from 0.12 to 0.13** -- Breaking changes in TLS defaults and redirect API. Review the changelog before upgrading.

5. **UPGRADE `rcgen` from 0.13 to 0.14** -- Wildcard SAN support (`*.localhost`) works via `SanType::DnsName`. The 0.14 API has improvements.

6. **BUILD custom `$VAR_NAME` interpolation for figment** -- Figment does not support `$VAR` substitution in TOML values natively. Implement a post-processing step or use the `Env` provider overlay pattern.

7. **RESOLVE ratatui version conflict** -- tui-logger 0.17.x requires ratatui 0.29, but you target ratatui 0.30. Either pin ratatui to 0.29, wait for tui-logger update, or implement a custom log widget.

8. **CLAIM `github.com/plug-mcp`** -- The GitHub organization is available and should be registered before someone else takes it.

---

## Recommended Cargo.toml Dependencies

```toml
[dependencies]
# Core MCP
rmcp = { version = "0.16", features = ["server"] }

# Async runtime
tokio = { version = "1.49", features = ["full"] }

# HTTP / Networking
axum = "0.8"
tower = "0.5"
tower-resilience = "0.7"  # circuit breaker, bulkhead, retry
reqwest = { version = "0.13", features = ["json", "rustls-tls"] }
rustls = "0.23"
rcgen = "0.14"

# TUI Dashboard
ratatui = { version = "0.29", features = ["crossterm_0_29"] }  # pin to 0.29 for tui-logger compat
crossterm = "0.29"
tui-logger = { version = "0.17", features = ["tracing-support"] }

# CLI
clap = { version = "4.5", features = ["derive"] }

# Configuration
figment = { version = "0.10", features = ["toml", "env"] }

# Concurrency
dashmap = "6.1"
arc-swap = "1.7"

# Serialization
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"

# Error handling
thiserror = "2.0"
anyhow = "1.0"

# Observability
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "fmt", "json"] }
tracing-appender = "0.2"

# File watching
notify = "8.2"

# Retry / Backoff
backon = "1.6"  # replaces unmaintained `backoff`
```

---

*Report generated 2026-03-03. All version numbers verified via web search against crates.io and associated documentation sites. Versions may have been updated since this report was generated; always verify with `cargo search <crate>` before finalizing.*
