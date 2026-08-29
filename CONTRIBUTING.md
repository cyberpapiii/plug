# Contributing To Plug

Plug is a Rust MCP multiplexer. Changes should preserve the operator model: one configured upstream set, many downstream clients, and multiplexor-owned routing/control surfaces.

## Workspace Layout

- `plug/`: CLI, daemon, IPC adapter, operator views, and install/runtime glue.
- `plug-core/`: MCP routing, transports, session handling, config, OAuth, enrichment, and shared types.
- `plug-test-harness/`: test helpers and local mock MCP servers.
- `docs/`: operator docs, plans, audits, and project-state records.
- `scripts/`: local development and reinstall helpers.

## Local Setup

For a fresh source checkout, run the isolated development setup in this order:

```sh
./scripts/setup-dev.sh
./scripts/setup-codesigning.sh
./scripts/dev-reinstall.sh
PLUG_DEV=1 plug-dev status
```

`scripts/setup-dev.sh` is idempotent and wires the workflow up: git hooks, a
check that GitHub auto-merge is enabled, and a check that `xcodegen` is present
for the app lane. Run it once per clone.

The development reinstall installs `~/.cargo/bin/plug-dev` and leaves the
production `plug` command owned by Plug.app unchanged. It also runs the
workspace check and `plug-core` tests; use `--quick` when those tests are not
needed.

Use `./scripts/dev-reinstall.sh --quick --clean` when you want to reinstall the local binary and immediately remove generated build artifacts.

## Required Checks

Run the local gate. It selects lanes with `scripts/classify-changes.sh`, the
same file CI's `classify` job runs, so a docs-only change runs nothing and a
`PlugApp/` change runs the app lane:

```sh
./scripts/dev.sh           # lanes your working tree touches, with tests
./scripts/dev.sh --quick   # formatting and lints only, about 20 seconds
./scripts/dev.sh --all     # rust and app lanes regardless of what changed
./scripts/dev.sh --e2e     # opt in to the Playwright browser lane
```

`scripts/setup-dev.sh` installs the hooks, after which this runs itself:
`pre-push` runs the quick gate, and `post-commit`, `post-merge` and
`post-checkout` run the artifact guard.

Bypass once with `git push --no-verify` or `PLUG_SKIP_HOOKS=1`; remove the hooks
with `git config --unset core.hooksPath`.

The underlying commands, if you would rather run them directly:

```sh
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo deny check advisories
```

The app lane runs the full `xcodebuild` PlugApp suite through
`scripts/test-app.sh`, which takes about 30 seconds and needs `xcodegen`
(`brew install xcodegen`). CI runs the same script with `--unsigned`, so a green
local app lane means a green `Test (Plug.app)` job. Do not call `xcodebuild`
directly: the suite compares the host bundle version against the embedded
daemon, and a bare run leaves that version empty and fails five fixture tests
with `NSCocoaErrorDomain Code=259`.

## Shipping A Change

```sh
./scripts/ship.sh "fix: stop the daemon racing its own socket"
```

That stages tracked edits, branches off `main` if you are on it, commits, pushes
through the pre-push gate, opens a pull request, and arms auto-merge. GitHub
merges it once CI is green and deletes the branch. Nothing to come back to.

It stages tracked modifications only. New files need an explicit `git add`,
because untracked files in this repo include private notes and local
credentials, and a script that swept them in would eventually publish one.

For distribution changes, also run:

```sh
dist plan --no-local-paths
dist build --artifacts=global
dist build --artifacts=local --target aarch64-apple-darwin
```

These commands can produce large local artifacts. The artifact guard handles the
routine case on its own; use `scripts/clean-build-artifacts.sh` to inspect
cleanup candidates and `scripts/clean-build-artifacts.sh --yes` after a release
pass when the local build output is no longer needed.

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
