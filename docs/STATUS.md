# Status

Open work only. `main` is what exists; `CHANGELOG.md` is what changed. Remove a
line here when it lands. Anything bigger than a line belongs in a GitHub issue.

## In progress

- Development pipeline simplification, September 2026: local build-and-try loop
  landed (`scripts/dev-install.sh`) and the unreferenced scripts, hooks, and
  CI jobs are cut; next is fixing the release cache miss and dropping Linux
  artifacts until a Linux user appears.

## Open

- App polish from daily use: copy, confusing states, recovery gaps. Fix as
  found; no sweep.
- `GetServerConfig` returns full credential material to any auth-token holder
  (#125). Decide whether to redact.
- Live downstream OAuth certification is done only for Claude Desktop. ChatGPT,
  Codex, Cursor, OpenCode, and a real WebKit platform-passkey ceremony are
  unproven.
- Doctor and unified snapshot code lives in `plug/src/commands/misc.rs`,
  `clients.rs`, and `plug-core/src/ipc.rs`; diagnosis wants its own module.
- The five signed PlugApp fixture tests run only on a Developer ID host, which
  today means one Mac.

## Deliberately not doing

- Fully live runtime reconfiguration.
- Modern-era follow-through behind gates (`subscriptions/listen`, mixed-era
  MRTR) until production clients speak MCP 2026.
- Homebrew formula and Linux tarballs, once removed, until someone asks.
