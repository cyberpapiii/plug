# Crate Stack

This is the current dependency picture for the code that exists today.

## Core Runtime

- `rmcp` 1.0.0
  Used for both upstream client connections and downstream MCP server handlers.

- `tokio`
  Shared async runtime across daemon, CLI, and HTTP paths.

- `serde`, `serde_json`, `serde_yml`
  Config, IPC, and MCP payload serialization.

- `anyhow`, `thiserror`
  Application and domain error handling.

## State And Concurrency

- `dashmap`
  Mutable concurrent state: health, circuit-breakers, semaphores, session registries.

- `arc-swap`
  Snapshot-style reads for config/tool cache.

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
  Config import/export and serialization.

- `directories`, `dirs`
  Config/runtime path discovery.

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

## Present But Not Active Product Surface

- `ratatui`
- `crossterm`
- `color-eyre`

These remain in the manifests from earlier TUI work, but there is no active TUI implementation in the current `v0.1` codepath.

## Notes

- The workspace currently pins `rmcp = 1.0.0` in `Cargo.toml`.
- A future update to `1.1.x` is planned, but not part of the `v0.1` stabilization gate.
