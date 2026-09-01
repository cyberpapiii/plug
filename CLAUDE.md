# CLAUDE.md

Plug is a Rust MCP multiplexer: one local config serves every AI client on the
Mac. `Plug.app` (SwiftUI, menu bar) bundles the daemon and owns its lifecycle
through SMAppService. The `plug` CLI talks to that daemon over a Unix socket
(`plug connect` for local stdio clients) and the daemon also serves remote
clients over HTTP. One person maintains it; keep it small and reliable.

## Layout

- `plug-core/` runtime, config, routing, transports, OAuth, doctor
- `plug/` CLI, daemon, IPC proxy
- `PlugApp/` the app; embeds the daemon binary at build time
- `plug-test-harness/` mock MCP server for tests
- `scripts/` the commands below plus release plumbing
- `docs/` user docs. `docs/archive/` is history; open it only when debugging
  something it documents.

## The loop

```sh
./scripts/dev-install.sh           build Plug.app from the tree and install it; the app swaps its daemon. 1-2 min
./scripts/dev.sh --quick           fmt + clippy, 15s. pre-push runs this
./scripts/dev.sh                   the lanes your change touches, with tests, about 2 min
./scripts/ship.sh "type: message"  commit, push, PR, auto-merge. Add new files with git add first
./scripts/release.sh [version]     only when asked to release. Write the CHANGELOG entry first
```

Try every change with `dev-install.sh` before shipping it. Do not call
`xcodebuild` directly. Do not start, kill, or restart the daemon from a shell:
a daemon started outside the login session hangs on the Keychain, and killing
one with live clients spawns competitors. Do not `git stash`.

## Rules

- `main` is truth. `CHANGELOG.md` is the record: every user-visible change
  adds a line under `[Unreleased]` in the same PR. No other doc needs updating.
- Open work lives in `docs/STATUS.md` and GitHub issues, nowhere else.
- Tests accompany behavior changes. Do not write tests for shell scripts.
- `rmcp` is pinned at 3.1.0. No `unsafe`. Bearer tokens compare in constant
  time. Daemon IPC is length-prefixed JSON over a Unix socket.
- Personal tool, not a platform: reliability over protocol surface, pass-through
  first, finish before widening.

## Gotchas

- Config `~/Library/Application Support/plug/config.toml`, logs
  `~/Library/Logs/plug/`, socket next to the config.
- The app and the CLI both verify `Plug.app` against the Developer ID team
  requirement. Never sign it with anything else.
- Untracked files in this repo include private notes and credentials. Never
  add them; `ship.sh` refuses to on purpose.
