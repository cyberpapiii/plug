# Plug 0.6.3

Plug 0.6.3 completes the menu bar app's one-time service handoff reliably on machines with active MCP clients.

## What changed

- Plug now pauses existing `plug connect` clients before replacing the legacy command-line LaunchAgent.
- Active clients can no longer recreate the old service during the ownership handoff.
- Clients resume automatically after the app-managed daemon is ready.

This is a focused reliability release. It includes the 0.6.2 launchd command-resolution fix and the complete 0.6.0 menu bar redesign.
