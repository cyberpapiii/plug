---
title: "refactor: Remove dead TUI dependencies"
type: refactor
status: completed
date: 2026-03-07
parent: Roadmap Tail Closeout
---

# refactor: Remove Dead TUI Dependencies

## Overview

Remove `ratatui`, `crossterm`, and `color-eyre` from the workspace `Cargo.toml`. These were declared for a TUI that was never built in the active codepath. No Rust source file imports them.

## Problem Statement / Motivation

The workspace manifest declares three dependencies with zero consumers:
- `ratatui = "0.30"` — TUI framework, no TUI exists
- `crossterm = { version = "0.29", features = ["event-stream"] }` — terminal backend for ratatui
- `color-eyre = "0.6"` — error reporting, replaced by `anyhow` throughout

These add confusion for contributors, inflate `cargo metadata`, and contradict the product posture ("CLI-first, not TUI-first").

## Proposed Solution

Remove all three from `[workspace.dependencies]` in the root `Cargo.toml`. Verify no crate references them. Run full quality checks.

## Implementation Tasks

- [x] Remove `ratatui`, `crossterm`, `color-eyre` from workspace `Cargo.toml`
- [x] `cargo check` passes
- [x] `cargo test` passes
- [x] `cargo clippy --all-targets --all-features -- -D warnings` passes
- [x] `cargo fmt --check` passes

## Acceptance Criteria

- [x] No workspace or crate `Cargo.toml` references `ratatui`, `crossterm`, or `color-eyre`
- [x] All quality checks pass
- [x] No Rust source file imports these crates

## Sources & References

- `docs/plans/2026-03-07-feat-roadmap-tail-closeout-plan.md` — parent plan, item 6
- `CLAUDE.md` — "The TUI is not part of the current product"
