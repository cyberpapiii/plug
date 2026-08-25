# Plug 0.5.2

Plug 0.5.2 completes the Mac app's first-run setup.

Choosing **Use Plug** now starts the background service bundled inside
Plug.app, keeps that service under the app's ownership, and preserves the same
configuration, servers, clients, and OAuth grants you already use.

The handoff waits for an older daemon to finish shutting down and resolves
reconnect races automatically, so no manual stop/start sequence is needed.

macOS may ask once to let the newly app-managed service use Plug's existing
Keychain entries. Choose **Always Allow** so future restarts stay quiet.

This release replaces 0.5.1 for Plug.app users. The CLI and MCP behavior are
otherwise unchanged.
