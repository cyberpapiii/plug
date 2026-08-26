# Plug 0.8.0 — A calmer Mac app

Plug 0.8.0 turns the menu-bar app into the everyday home for Plug. The command
line remains fully supported for agents, while people can now understand and
manage the same system without translating terminal output.

## What feels different

- **One clear status.** The menu bar, popover, and main window use the same
  plain-language verdict, so Plug no longer looks healthy in one place and
  broken in another.
- **Useful at a glance.** Open the menu-bar popover to see whether Plug is
  working, which server needs attention, and the one action that fixes it.
- **Four focused workspaces.** Servers, Tools, Connections, and Activity answer
  the common questions without exposing MCP or launchd terminology.
- **Real server and tool control.** Add, import, edit, enable, disable, sign in,
  sign out, search tools, or turn individual tools on and off from the app.
- **See what each app can use.** Connections shows linked and live clients,
  including Claude, Cursor, Codex, and other detected apps.
- **Calls are understandable.** Activity names the client, tool, server,
  outcome, and timing instead of showing a generic `tools/call` entry.
- **Settings are finally complete.** Login behavior, notifications, updates,
  service controls, checkup, logs, version information, and Quit are easy to
  find.

## Reliability improvements

- Editing a server preserves its complete existing configuration, including
  advanced OAuth, timeout, tool-group, sandbox, environment, and credential
  fields that the compact form does not show.
- Activity fetches the newest calls incrementally and says when older retained
  history has actually been trimmed.
- The app refreshes quickly while it is visible, slows down in the background,
  and does not download the full tool catalog every two seconds.
- App updates correctly recognize and replace an older macOS background-service
  registration instead of treating Plug's own daemon as unknown software.
- All status is communicated with a symbol and words, not color alone.

## Design

Plug uses native macOS controls, real app icons, restrained Liquid Glass on
compact actions and transient surfaces, and quiet content lists. Healthy state
stays visually calm; problems move to the top and carry an immediate action.

## For agents

The CLI and app remain peers over one daemon. Operator IPC v6 adds authenticated
server-definition reads for safe editing, plus tool mutation and richer activity
attribution. Existing agent workflows continue through the same `plug` command
and shared configuration.

## Compatibility

- macOS 14 or later
- Native Liquid Glass on macOS 26; native material fallback on macOS 14 and 15
- Existing Plug configuration, OAuth grants, linked clients, and server data
  are preserved during the update

