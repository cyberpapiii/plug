---
title: Downstream HTTPS serving
date: 2026-03-07
category: integration-issues
components:
  - plug/src/runtime.rs
  - plug/src/main.rs
  - plug-core/src/config/mod.rs
  - plug-core/src/tls.rs
problem_type: downstream-transport-security
summary: Added optional HTTPS termination to `plug serve` with cert/key configuration, ring-backed rustls provider installation, and a real TLS MCP regression covering initialize, tools/list, and SSE attach.
related:
  - docs/brainstorms/2026-03-07-downstream-https-serving-brainstorm.md
  - docs/plans/2026-03-07-feat-downstream-https-serving-plan.md
---

# Downstream HTTPS Serving

## Problem

`plug serve` exposed downstream Streamable HTTP without TLS termination. That was acceptable for localhost, but it was not acceptable for real remote use: auth tokens, session IDs, and MCP payloads would cross the network in cleartext.

The codebase also had no explicit operator configuration for certificates or keys, and the existing HTTP tests only exercised in-process router behavior rather than a real TLS-wrapped downstream MCP flow.

## Constraints

- Keep the shared engine/router/session model unchanged.
- Keep TLS concerns at the transport edge.
- Do not introduce ACME/Let's Encrypt automation in the first tranche.
- Do not reintroduce OpenSSL/native-tls for either the downstream server path or the upstream HTTP transport path.

## Solution

### 1. Add downstream TLS config to `HttpConfig`

In [plug-core/src/config/mod.rs](/Users/robdezendorf/.config/superpowers/worktrees/plug/feat-downstream-https-serving/plug-core/src/config/mod.rs), add:
- `tls_cert_path: Option<PathBuf>`
- `tls_key_path: Option<PathBuf>`

Validation now enforces:
- cert and key must be configured together
- both files must exist
- both files must be readable
- the key file must not be group/world readable on Unix
- non-loopback downstream binds require TLS

That last rule is important: the feature should not allow accidental `0.0.0.0` cleartext serving by configuration.

### 2. Add a shared rustls provider helper

A new helper lives in [plug-core/src/tls.rs](/Users/robdezendorf/.config/superpowers/worktrees/plug/feat-downstream-https-serving/plug-core/src/tls.rs):

```rust
pub fn ensure_rustls_provider_installed() {
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}
```

This gives the process a single ring-backed rustls bootstrap convention instead of hiding provider installation inside one specific transport path. The branch also aligns rmcp's upstream HTTP client path to `reqwest-tls-no-provider`, so the same provider policy now applies in both directions.

### 3. Install the provider at process startup and use HTTPS only at the serve edge

In [plug/src/main.rs](/Users/robdezendorf/.config/superpowers/worktrees/plug/feat-downstream-https-serving/plug/src/main.rs), `plug_core::tls::ensure_rustls_provider_installed()` now runs at startup.

In [plug/src/runtime.rs](/Users/robdezendorf/.config/superpowers/worktrees/plug/feat-downstream-https-serving/plug/src/runtime.rs), `cmd_serve()` was refactored so the transport-edge helper decides between:
- plain HTTP via `axum_server::bind(...)`
- HTTPS via `axum_server::bind_rustls(...)`

The router itself remains unchanged. `build_router(...)` still owns protocol behavior; socket/TLS binding stays in runtime/bootstrap code.

### 4. Prove the real downstream TLS flow, not just a health endpoint

The final regression test does three things over HTTPS against the real downstream MCP router:
- `initialize`
- `tools/list`
- `GET /mcp` SSE attach, asserting the priming event is delivered over TLS

That matters because a POST-only test would not prove the server-initiated downstream stream path.

### 5. Stabilize unrelated inherited test noise so the tranche can ship

This branch inherited the pre-existing `daemon_backed_proxy_recovers_after_daemon_restart` flake from `main`. The fix was to prebuild `mock-mcp-server` once and execute the binary directly in the test, avoiding a cold `cargo run` inside the readiness deadline.

## Verification

These passed on the finished branch:

```bash
cargo fmt --check
cargo deny check licenses
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

The downstream HTTPS regression now proves:
- TLS handshake succeeds
- MCP initialize works over HTTPS
- session-bearing MCP requests work over HTTPS
- SSE attach works over HTTPS

## Why This Shape

The important design choice was to keep TLS as a runtime/bootstrap concern.

The router and session model did not need to know whether the socket was HTTP or HTTPS. That kept the change small and avoided contaminating the shared MCP protocol handling with serving details.

## Prevention

When adding transport security to `plug` in the future:

1. Keep upstream and downstream TLS dependency choices aligned; otherwise one path will quietly keep dragging in native crypto.
2. Add config validation that blocks unsafe remote defaults.
3. Test the real network path, not only in-process router logic.
4. For MCP over HTTPS, cover both request/response and SSE attach.
5. Keep TLS termination at the serving edge unless protocol logic truly depends on it.
6. If a branch inherits a known flake from `main`, stabilize it before claiming the new feature is finished.
