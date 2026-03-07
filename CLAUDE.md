# CLAUDE.md — plug

## What This Project Is

`plug` is a daemon-backed MCP multiplexer written in Rust. It sits between AI clients and MCP servers so one local config and one shared runtime can serve many clients without duplicated server processes or duplicated setup.

Current product surface:

- a shared core runtime in `plug-core`
- a guided CLI in `plug`
- structured JSON output for agent use
- downstream stdio via `plug connect`
- downstream Streamable HTTP via `plug serve`

The TUI is not part of the current product. Some TUI-era dependencies remain in the manifests, but there is no ratatui implementation in the active codepath.

## Current Status

This is an active codebase, not a pre-development repo.

Implemented today:

- upstream stdio and HTTP connections
- merged tool routing with client-aware filtering
- daemon + Unix socket IPC
- HTTP server for downstream clients
- import/export/doctor flows
- startup recovery and health monitoring

Still incomplete:

- full notification forwarding
- cancellation/progress passthrough
- full resources/prompts forwarding
- fully live runtime reconfiguration

## Documentation Map

Use these as the current source of truth:

- `docs/VISION.md`
- `docs/UX-DESIGN.md`
- `docs/ARCHITECTURE.md`
- `docs/PLAN.md`
- `docs/RISKS.md`
- `docs/CRATE-STACK.md`

Strategy / planning docs:

- `docs/plans/2026-03-06-strategic-assessment.md`
- `docs/plans/2026-03-06-v0-1-stabilization-execution-plan.md`
- `todos/029-032`

## Tech Stack

- Rust 2024 edition
- `rmcp` 1.0.0
- Tokio
- Axum
- DashMap
- ArcSwap
- Clap
- Figment
- `backon`
- `notify` + `notify-debouncer-mini`
- `tracing` + `tracing-subscriber` + `tracing-appender`

## Development Commands

```bash
cargo check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --check
```

## Project Structure

```text
Cargo.toml          workspace root
plug-core/          shared runtime, config, routing, HTTP, doctor, import/export
plug/               CLI, daemon, IPC proxy, views
plug-test-harness/  integration test support
docs/               product docs, plans, research
todos/              tracked work items
```

## Product Posture

- personal tool, not enterprise control plane
- CLI-first, not TUI-first
- pass-through first, selective value-add second
- reliability over protocol surface area
- finish and stabilize before widening scope
