---
title: feat: downstream https serving
status: completed
date: 2026-03-07
origin: docs/brainstorms/2026-03-07-downstream-https-serving-brainstorm.md
---

# feat: downstream https serving

## Overview

Add HTTPS termination to `plug serve` so remote clients can connect securely to the downstream MCP server.

## Problem Statement / Motivation

`plug serve` currently exposes downstream Streamable HTTP without TLS termination. That is acceptable for localhost development but not for real remote use. The roadmap and product direction assume remote clients can connect securely over the internet, which requires HTTPS.

## Proposed Solution

- [x] Add downstream TLS configuration fields for certificate and private key paths
- [x] Add a shared rustls provider initialization helper usable by both outbound and inbound TLS code
- [x] Introduce a TLS-enabled Axum serving path for `plug serve`
- [x] Preserve the existing non-TLS HTTP serving path when TLS is not configured
- [x] Add HTTPS regression/integration coverage for downstream initialize + request handling
- [x] Update docs and operational notes for downstream HTTPS configuration
- [x] Run full verification and ship in a dedicated feature branch

## Technical Considerations

- Use rustls/ring, not OpenSSL/native-tls.
- Avoid mixing the PR 21 portability fix with this downstream feature implementation.
- Prefer certificate/key file configuration for the first tranche; defer ACME automation.
- The serving abstraction should make HTTP vs HTTPS a transport-edge concern, not a rewrite of the shared engine or session model.

## System-Wide Impact

- **Interaction graph**: CLI/config -> runtime startup -> HTTP server bootstrap -> downstream session establishment -> shared engine/tool router.
- **Error propagation**: bad cert/key paths or TLS bootstrap failures must surface as clear startup errors, not silent runtime degradation.
- **State lifecycle risks**: TLS config is startup-only; no new persistent state beyond config validation.
- **API surface parity**: HTTPS must preserve the same downstream protocol surface as current HTTP.
- **Integration test scenarios**: HTTPS initialize, SSE/stream attach, authenticated downstream request path, and fallback to plain HTTP when TLS is disabled.

## Acceptance Criteria

- [x] `plug serve` accepts TLS configuration and starts HTTPS successfully with valid cert/key files
- [x] remote clients can complete MCP initialization over HTTPS
- [x] downstream request handling behaves the same over HTTP and HTTPS
- [x] startup fails cleanly with invalid TLS configuration
- [x] local verification passes: fmt, tests, clippy

## Sources & References

- **Origin brainstorm:** [docs/brainstorms/2026-03-07-downstream-https-serving-brainstorm.md](docs/brainstorms/2026-03-07-downstream-https-serving-brainstorm.md)
- Current architecture surface: [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)
- Existing HTTP downstream implementation: [plug-core/src/http/server.rs](/Users/robdezendorf/.config/superpowers/worktrees/plug/feat-downstream-https-serving/plug-core/src/http/server.rs)
- Existing runtime entrypoint: [plug/src/main.rs](/Users/robdezendorf/.config/superpowers/worktrees/plug/feat-downstream-https-serving/plug/src/main.rs)
- Existing HTTP server bootstrap plan context: [docs/plans/2026-03-03-feat-phase-2-http-portless-plan.md](docs/plans/2026-03-03-feat-phase-2-http-portless-plan.md)
