---
title: "Dead TUI Dependencies in Workspace Manifest"
category: build-errors
tags:
  - cargo
  - dependencies
  - cleanup
  - tui
  - workspace
module: Cargo.toml (workspace root)
symptom: "Workspace Cargo.toml declares ratatui, crossterm, and color-eyre with zero consumers"
root_cause: "TUI was planned (Phase 4) but never built; dependencies were declared speculatively and never removed"
date: 2026-03-07
pr: "#29"
phase: "Roadmap Tail Closeout"
---

# Dead TUI Dependencies in Workspace Manifest

## Problem Statement

The workspace root `Cargo.toml` declared three dependencies under a `# TUI (Phase 4)` comment:
- `ratatui = "0.30"`
- `crossterm = { version = "0.29", features = ["event-stream"] }`
- `color-eyre = "0.6"`

No crate `Cargo.toml` referenced them. No `.rs` source file imported them. They were speculative declarations for a TUI that was never built.

## Investigation Steps

1. Grepped all `**/Cargo.toml` files for `ratatui`, `crossterm`, `color-eyre` — only found in workspace root
2. Grepped all `.rs` files for the same — zero matches
3. Confirmed `CLAUDE.md` explicitly states: "The TUI is not part of the current product"
4. Confirmed no transitive dependency from any active crate

## Solution

### Root Cause Analysis

Dependencies were added during early planning when a TUI was envisioned as Phase 4. The product direction shifted to "CLI-first, not TUI-first" but the manifest was never cleaned up.

### Working Solution

Removed the three lines and their section comment from `[workspace.dependencies]` in the root `Cargo.toml`. No other changes needed — no crate referenced them.

```diff
-# TUI (Phase 4)
-ratatui = "0.30"
-crossterm = { version = "0.29", features = ["event-stream"] }
-color-eyre = "0.6"
```

All quality checks pass (`cargo check`, `cargo test`, `cargo clippy`, `cargo fmt`).

## Prevention

- Don't declare speculative workspace dependencies before a crate actually needs them
- When product direction changes, audit the manifest for orphaned declarations
- Periodic `cargo machete` or similar unused-dependency detection

## Related Documentation

- `docs/plans/2026-03-07-feat-roadmap-tail-closeout-plan.md` — parent plan, item 6
- `CLAUDE.md` — product posture documentation

### PRs

- PR #29 — Dead TUI dependency removal
