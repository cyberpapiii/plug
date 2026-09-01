---
title: fix: review fixes tls backend portability
status: completed
date: 2026-03-07
origin: docs/brainstorms/2026-03-07-review-fixes-tls-backend-portability-brainstorm.md
---

# fix: review fixes tls backend portability

## Overview

Adjust PR 21's outbound HTTP TLS backend so the branch remains functionally correct for HTTPS upstreams while also passing license and cross-target CI gates.

## Problem Statement / Motivation

The current PR 21 branch replaced `reqwest-native-tls` with rmcp's default rustls path. That removed `openssl-sys`, but the replacement still pulls `aws-lc-sys` and `webpki-root-certs`, which leaves the branch failing `cargo deny` and cross-target CI. We need a narrower rustls configuration with an explicit provider choice.

## Proposed Solution

- [x] Switch rmcp from `reqwest` to `reqwest-tls-no-provider`
- [x] Add direct `rustls` dependency with `ring` support and install the provider at the HTTP upstream connection boundary
- [x] Update `deny.toml` only for `CDLA-Permissive-2.0` if it remains required after the dependency change
- [x] Add or update regression coverage so authenticated HTTP upstream setup still works after the provider change
- [x] Run full local verification (`cargo deny`, `cargo test`, `cargo clippy`, `cargo fmt --check`)
- [x] Update this plan to completed once the branch is green

## Technical Considerations

- The only real outbound HTTP upstream path today is in `plug-core/src/server/mod.rs`; install the crypto provider there rather than in binary startup code.
- Avoid provider installation code that panics if called repeatedly. It must be safe for tests, daemon paths, and reconnect flows.
- Keep the license policy tight: accept the uncommon cert bundle license only if necessary, but do not allow OpenSSL.

## System-Wide Impact

- **Interaction graph**: `ServerManager::initialize_server()` creates the HTTP upstream transport, which is used by direct runtime startup, daemon startup, and reconnect flows.
- **Error propagation**: a bad TLS setup fails server initialization and surfaces as an upstream connection failure. Keep the error path explicit.
- **State lifecycle risks**: this change touches connection setup only; no persisted state is introduced.
- **API surface parity**: the change affects all HTTP upstream users automatically because there is a single connection path.
- **Integration test scenarios**: authenticated HTTP upstream connection setup should still produce a valid bearer header and complete initialization.

## Sources & References

- **Origin brainstorm:** [docs/brainstorms/2026-03-07-review-fixes-tls-backend-portability-brainstorm.md](docs/brainstorms/2026-03-07-review-fixes-tls-backend-portability-brainstorm.md)
- Current HTTP upstream transport: [plug-core/src/server/mod.rs](/Users/robdezendorf/.config/superpowers/worktrees/plug/feat-review-fixes-critical/plug-core/src/server/mod.rs)
- Current license policy: [deny.toml](/Users/robdezendorf/.config/superpowers/worktrees/plug/feat-review-fixes-critical/deny.toml)
- PR under repair: https://github.com/cyberpapiii/plug/pull/21
