# Crate Stack

This document reflects the dependency picture of the current merged codebase.

`CHANGELOG.md` and `docs/STATUS.md` carry project state. This file is
about dependency shape, not roadmap status.

## Off-Main OAuth Owner Candidate

The following dependency changes `exist off-main` on
`codex/oauth-owner-passkey-implementation`; they are not part of the current
merged dependency picture:

- `passkey-auth` `=0.1.3`
  Exact-pinned WebAuthn server ceremonies with serializable registration and
  authentication state for restart-safe owner enrollment and approval.

- `hmac` `0.12`
  HMAC-SHA256 request proofs for local operator endpoints, keeping the reusable
  operator token out of HTTP requests.

- test-only `ciborium`, `ed25519-dalek`, and `filetime`
  Browser-authenticator fixtures, signed metadata fixtures, and persistence
  timestamp tests.

The locked candidate graph passes Rust 1.88, `cargo deny`, RustSec, Trivy, and
both Linux CI target checks. Dependency guards confirm that `openssl-sys`,
`aws-lc-sys`, `native-tls`, and `rsa` are absent.

## Core Runtime

- `rmcp` `=3.1.0`
  MCP protocol implementation for both downstream server handlers and upstream client sessions.
  Version policy: exact pin. RMCP releases can change model types, negotiated
  protocol behavior, and transport helper contracts, so every upgrade is a
  deliberate manifest edit with focused compatibility tests and the full
  workspace suite.

- `tokio`
  Shared async runtime across daemon, stdio proxying, and HTTP serving.

- `serde`, `serde_json`, `serde_norway`
  Config, IPC, and MCP payload serialization.

- `anyhow`, `thiserror`
  Application and domain error handling.

## State And Concurrency

- `dashmap`
  Mutable concurrent state such as health, circuit breakers, semaphores, and stateful session
  storage.

- `arc-swap`
  Snapshot-style reads for config and routing state.

- `uuid`
  Session IDs and logical client IDs.

- `url`
  URL parsing and percent-encoded form serialization for downstream OAuth
  redirect-URI allowlist validation and authorize-redirect construction.

## HTTP And Transport

- `axum`
  Downstream HTTP server.

- `tower`, `tower-http`
  HTTP middleware and request handling support.

- `tokio-stream`, `async-stream`, `tokio-util`, `http`, `bytes`
  SSE and async transport plumbing.

## Config / Files / Paths

- `figment`
  Layered config loading.

- `toml`
  Config serialization and import/export.

- `directories`, `dirs`
  Runtime/config path discovery.

- `notify`, `notify-debouncer-mini`
  Config file watching.

## Reliability / Runtime

- `backon`
  Retry/backoff support.

- `rand`
  Jitter and token generation support.

- `tracing`, `tracing-subscriber`, `tracing-appender`
  Structured logging and daemon log files.

- `fs4`
  PID file locking.

- `subtle`, `hex`
  Auth token generation and constant-time comparison helpers.

## CLI

- `clap`
  Command parsing.

- `dialoguer`, `console`, `open`
  Guided CLI flows and local config opening.

## Removed From The Current Product Surface

- `ratatui`
- `crossterm`
- `color-eyre`

These old TUI dependencies have been removed from the active manifests. The current merged codebase
has no TUI product surface.
