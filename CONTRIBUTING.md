# Contributing To Plug

Plug is a Rust MCP multiplexer. Changes should preserve the operator model: one configured upstream set, many downstream clients, and multiplexor-owned routing/control surfaces.

## Workspace Layout

- `plug/`: CLI, daemon, IPC adapter, operator views, and install/runtime glue.
- `plug-core/`: MCP routing, transports, session handling, config, OAuth, enrichment, and shared types.
- `plug-test-harness/`: test helpers and local mock MCP servers.
- `docs/`: operator docs, plans, audits, and project-state records.
- `scripts/`: local development and reinstall helpers.

## Local Setup

```sh
cargo check --workspace
./scripts/dev-reinstall.sh --quick
plug status
```

The development reinstall keeps the PATH binary at `~/.cargo/bin/plug` and normalizes `~/.local/bin/plug` to point at it.

Use `./scripts/dev-reinstall.sh --quick --clean` when you want to reinstall the local binary and immediately remove generated build artifacts.

## Required Checks

Run these before opening a PR:

```sh
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo deny check advisories
```

For distribution changes, also run:

```sh
dist plan --no-local-paths
dist build --artifacts=global
dist build --artifacts=local --target aarch64-apple-darwin
```

These commands can produce large local artifacts. Use `scripts/clean-build-artifacts.sh` to inspect generated cleanup candidates and `scripts/clean-build-artifacts.sh --yes` after a release pass when the local build output is no longer needed.

## Multiplexor Mental Model

Plug is not a leaf MCP server. It owns behavior that a normal single server does not:

- Capability synthesis across upstream servers.
- Namespaced routing and name-collision handling.
- Lazy tool discovery modes for clients with different tool-surface constraints.
- Reverse-request routing for sampling, elicitation, roots, progress, and cancellation.
- Task ownership and request lifecycle tracking.
- Artifact spillover through `plug://artifact/...`.
- Daemon IPC for stdio client adapters.
- Operator inventory and trust/risk metadata.

Do not replace these with SDK defaults unless the change preserves Plug's multiplexor control surface and has parity tests.

## PR Expectations

Every behavior change should include tests. Small changes still need focused coverage when they touch protocol behavior, config parsing, auth, routing, IPC, transport sessions, or public docs.

For roadmap-relevant work:

- Verify the implementation on current `main`; plans and branch summaries are not proof.
- Update `docs/PROJECT-STATE-SNAPSHOT.md` and `docs/PLAN.md` when project state changes.
- Keep `docs/audit-2026-05-17.md` execution status current when addressing audit rows.
- Add a short entry to `docs/hardening-log.md` for hardening work.

Wire-level compatibility matters. If a change can break Claude Code, Cursor, Codex, OpenCode, Windsurf, VS Code Copilot, Zed, Gemini CLI, or another documented client target, make it backward-compatible behind configuration or document the migration explicitly.
