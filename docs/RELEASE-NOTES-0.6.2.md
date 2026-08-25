# Plug 0.6.2

Plug's app-owned background service now starts command-based MCP servers with
the same executable lookup a Terminal session uses.

## What changed

- Fixed `node`, `npx`, and other user-installed commands appearing unavailable
  after choosing **Use Plug** in the macOS app.
- Bare stdio commands first use the daemon's current `PATH`, then safely resolve
  through the user's macOS login shell when launchd supplied only its minimal
  system path.
- `plug doctor` now validates commands with the same lookup used by the live
  runtime, so its answer cannot disagree with app-owned daemon behavior.

This is a compatibility repair for the 0.6 app-owned launchd lifecycle. It does
not change configured commands, arguments, credentials, or protocol behavior.
