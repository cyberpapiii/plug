# CLAUDE.md — plug

This repo uses **Compound Engineering (CE)** as its workflow operating system.

This file is a repo-local CE adapter. It explains how to use CE safely here. It is not a parallel
workflow.

## What This Project Is

`plug` is a daemon-capable MCP multiplexer written in Rust. It sits between AI clients and MCP servers so one local config can serve many clients without duplicated setup. Local stdio clients normally share the daemon runtime; `plug serve` starts its own engine for downstream HTTP access unless you explicitly run daemon mode.

Current product surface:

- a shared core runtime in `plug-core`
- a guided CLI in `plug`
- structured JSON output for agent use
- downstream stdio via `plug connect`
- downstream Streamable HTTP via `plug serve`

The TUI is not part of the current product, and the old TUI dependencies have been removed from the
active manifests.

## Current Status

This is an active codebase, not a pre-development repo.

Implemented today:

- upstream stdio, HTTP, and legacy SSE connections (with HTTP→SSE auto-fallback)
- merged tool routing with client-aware filtering
- daemon + Unix socket IPC
- HTTP server for downstream clients
- downstream HTTPS and bearer auth for non-loopback HTTP
- logging forwarding
- tools/resource/prompt list_changed forwarding for stdio + HTTP
- progress and cancellation routing for stdio + HTTP
- resources/prompts/templates forwarding
- resource subscribe/unsubscribe lifecycle
- completion forwarding across stdio, HTTP, and daemon IPC
- meta-tool mode
- roots forwarding with union cache across all transports
- elicitation + sampling reverse-request forwarding across stdio, HTTP, and daemon IPC
- import/export/doctor flows
- startup recovery and health monitoring

Still incomplete:

- OAuth 2.1 + PKCE for upstream remote servers
- daemon IPC notification parity beyond logging
- dedicated end-to-end tests for `structuredContent` and `resource_link`
- HTTP elicitation timeout (todo 045 — deferred, needs plan revision)
- fully live runtime reconfiguration

## Documentation Map

Use these as the current source of truth:

- `docs/PROJECT-STATE-SNAPSHOT.md`
- `docs/TRUTH-RULES.md`
- `docs/VISION.md`
- `docs/UX-DESIGN.md`
- `docs/ARCHITECTURE.md`
- `docs/PLAN.md`
- `docs/ROADMAP-AUDIT-2026-03-08.md`
- `docs/RISKS.md`
- `docs/CRATE-STACK.md`

Agent workflow guide:

- `AGENTS.md`
- `docs/WORKFLOW-OPERATING-MODEL.md`

Strategy / planning docs:

- `docs/plans/2026-03-06-strategic-assessment.md`
- `docs/plans/2026-03-06-v0-1-stabilization-execution-plan.md`
- `todos/029-032`

## Truth Workflow

Before answering any question about project progress, roadmap state, or what is implemented:

1. Read `docs/PROJECT-STATE-SNAPSHOT.md`
2. Read `docs/PLAN.md` if more detail is needed
3. Verify against code on `main` if the answer materially matters

Do not answer from branch summaries, PR descriptions, plan docs, or prior agent outputs alone.

Truth rules:

- `main` is the only source of “done now”
- branch/worktree code is never “done now” until merged to `main`
- plans describe intended work, not current truth
- historical/research/solution docs are compound knowledge, not current truth

Use these labels for roadmap-relevant features:

- `done on main`
- `partial on main`
- `exists off-main`
- `missing`

If uncertain, prefer `exists off-main` or `missing`, never `done on main`.

## Repo-Specific CE Gotchas

- there are many stale worktrees from prior development; do not confuse worktree state with `main`
- `fix/subscription-rebind-confidence` is an extraction source, not a truth source
- older `docs/plans/*` files may still be useful, but many are historical planning context
- roadmap and progress answers must start from the snapshot, not from historical plans

## Subagents

Subagents are encouraged for bounded research, verification, review, branch/worktree audits, and
git archaeology. This protects the main agent’s context window.

Rules:

- subagents gather evidence; they do not make final truth decisions
- final status labels are assigned only in the main thread
- every subagent finding should be framed as:
  - verified on `main`
  - verified off-main
  - inferred

Prefer one subagent per bounded question over large undifferentiated swarms.

## Post-Merge Truth Pass

Every roadmap-affecting PR should complete this checklist after merge:

- [ ] merged code exists on `main`
- [ ] `docs/PROJECT-STATE-SNAPSHOT.md` still matches `main`
- [ ] `docs/PLAN.md` still matches `main`
- [ ] branch-only wording removed or explicitly retained as branch-scoped
- [ ] remaining-work lists revalidated

## Tech Stack

- Rust 2024 edition
- `rmcp` 1.1.0
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
