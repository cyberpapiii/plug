# Contributing

Plug is a Rust MCP multiplexer with a macOS app that owns the daemon. The
layout, the commands, and the rules are in `CLAUDE.md`; it is short and it is
written for people as much as for agents.

## Try a change

```sh
./scripts/dev-install.sh
```

Builds `Plug.app` from your working tree, signs it with the Developer ID in
your login keychain, installs it over `/Applications/Plug.app`, and lets the
app replace its daemon. About two minutes cold, seconds warm, no network.
Needs `xcodegen` (`brew install xcodegen`) and a Developer ID certificate for
team `HJF7LN64XX`; without one, `./scripts/dev.sh` still builds and tests
everything.

## Check a change

```sh
./scripts/dev.sh           # the lanes your change touches, with tests
./scripts/dev.sh --quick   # fmt and clippy, what pre-push runs
./scripts/dev.sh --all     # every lane
```

`./scripts/setup-dev.sh` installs the git hooks once per clone.

## Ship a change

```sh
./scripts/ship.sh "fix: what changed"
```

Commits tracked edits, pushes through the pre-push gate, opens a pull request,
and arms auto-merge. Untracked files are never staged; `git add` them first.
Every user-visible change adds a line under `[Unreleased]` in `CHANGELOG.md`.

## Expectations

- Behavior changes come with tests. Protocol, config, auth, routing, IPC, and
  transport changes need focused coverage.
- Plug is a multiplexer, not a leaf server. Keep capability synthesis,
  namespaced routing, reverse-request routing, and daemon IPC intact; do not
  swap them for SDK defaults.
- A change that can break a documented client (Claude Code, Claude Desktop,
  Cursor, Codex, Zed, Gemini CLI, VS Code Copilot, and the rest in
  `docs/CLIENT-COMPAT.md`) stays backward-compatible behind configuration or
  documents the migration.
