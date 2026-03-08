# Crate Stack

This document reflects the dependency picture of the current merged codebase.

Use `docs/PROJECT-STATE-SNAPSHOT.md` and `docs/PLAN.md` for overall project state. This file is
about dependency shape, not roadmap status.

## Core Runtime

- `rmcp` 1.1.x
  MCP protocol implementation for both downstream server handlers and upstream client sessions.

- `tokio`
  Shared async runtime across daemon, stdio proxying, and HTTP serving.

- `serde`, `serde_json`, `serde_yml`
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

- `fs2`
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
