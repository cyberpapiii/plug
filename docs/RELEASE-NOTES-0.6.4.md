# Plug 0.6.4

Plug 0.6.4 makes app-managed MCP servers inherit the same command environment as your login shell.

## What changed

- Script-based commands such as `npx` can now find their interpreters when Plug.app owns the daemon.
- Bare commands and their child processes share one resolved login-shell `PATH`.
- The environment is resolved once per daemon, keeping startup predictable and quiet.

This completes the service migration work begun in 0.6.2 and 0.6.3 without adding server-specific configuration.
