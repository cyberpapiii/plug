---
title: Review fixes TLS backend portability
date: 2026-03-07
category: integration-issues
components:
  - plug-core/src/server/mod.rs
  - Cargo.toml
  - deny.toml
problem_type: dependency-runtime-portability
summary: Replaced rmcp's default rustls/aws-lc HTTP client path with rustls-no-provider plus an explicit ring provider install so PR 21 could pass license and cross-target CI without reintroducing OpenSSL.
related:
  - docs/brainstorms/2026-03-07-review-fixes-tls-backend-portability-brainstorm.md
  - docs/plans/2026-03-07-fix-review-fixes-tls-backend-portability-plan.md
  - https://github.com/cyberpapiii/plug/pull/21
---

# Review Fixes TLS Backend Portability

## Symptom

PR 21 fixed the code-review findings but still failed CI after the first TLS adjustment:
- `cargo deny check licenses` rejected `aws-lc-sys` and `webpki-root-certs`
- cross-target checks still failed because the selected TLS stack required native crypto compilation

The original `reqwest-native-tls` path had already been removed because it pulled `openssl-sys`, so simply reverting was not acceptable.

## Root Cause

rmcp exposes multiple reqwest-backed HTTP transport feature paths:
- `reqwest-native-tls` routes through OpenSSL/native TLS
- `reqwest` routes through reqwest's default rustls stack, which in practice pulled the aws-lc provider
- `reqwest-tls-no-provider` wires rustls transport support without choosing a crypto provider

The intermediate PR 21 change switched from native TLS to rmcp's `reqwest` feature. That removed `openssl-sys`, but it still selected a rustls path that brought in `aws-lc-sys`, leaving both portability and license policy problems unsolved.

## Working Solution

### 1. Use the no-provider rustls path in rmcp

In the workspace `Cargo.toml`, replace:

```toml
"reqwest"
```

with:

```toml
"reqwest-tls-no-provider"
```

This keeps rmcp's streamable HTTP client transport but stops it from silently choosing aws-lc.

### 2. Add a direct rustls dependency with ring

Add a workspace dependency:

```toml
rustls = { version = "0.23", default-features = false, features = ["std", "ring"] }
```

and consume it from `plug-core`.

This makes the provider choice explicit and keeps the crypto backend on a license/build profile the project accepts.

### 3. Install the provider at the real HTTP connection boundary

In `plug-core/src/server/mod.rs`, install the provider immediately before creating the HTTP upstream transport:

```rust
fn ensure_rustls_provider_installed() {
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }
}
```

and call it inside the `TransportType::Http` arm before `StreamableHttpClientTransport::from_config(...)`.

This matters because the HTTP upstream connection path is shared by:
- direct runtime startup
- daemon startup
- reconnect flows
- tests that exercise HTTP upstream initialization without going through `main`

Installing at the transport boundary covers all of those paths without introducing extra global binary startup behavior.

### 4. Allow only the remaining cert-bundle license

After the provider switch, `cargo deny` no longer needs `OpenSSL`, but it still needs:

```toml
"CDLA-Permissive-2.0"
```

for the cert-bundle path used by the rustls stack.

That was added to `deny.toml`, and no broader license widening was necessary.

## Verification

These checks passed after the fix:

```bash
cargo deny check licenses
cargo fmt --check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```

The dependency tree also confirmed the intended outcome:
- no `aws-lc-sys`
- no `openssl-sys`
- no `native-tls`

## Why This Is the Right Fix

This was not just a CI band-aid. The TLS backend choice affects:
- whether HTTPS upstreams can initialize reliably on all supported targets
- what native build toolchains are required
- what license policy the repo must accept

The final setup keeps HTTPS upstream support while staying off both OpenSSL and aws-lc.

## Prevention

When changing rmcp/reqwest TLS features in this repo:

1. Check `cargo tree -i aws-lc-sys`, `cargo tree -i openssl-sys`, and `cargo tree -i native-tls`
2. Run `cargo deny check licenses` before assuming a feature swap is safe
3. If using a no-provider rustls path, install the provider where the actual HTTP transport is constructed, not only in binary startup
4. Keep the allowlist minimal; accept only licenses that remain necessary after the dependency graph is corrected

## Related Lessons

This fix pairs with the broader PR 21 review-fix work:
- authenticated HTTP upstream bearer-header construction must use `SecretString::as_str()`
- daemon IPC parity fixes should not regress portability work
- CI portability issues are often dependency-policy issues, not only workflow config issues
