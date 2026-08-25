# Plug 0.5.0

Plug 0.5.0 gives humans a proper Mac app without turning Plug into two products.
The new menu-bar app and the CLI are equal clients of the same daemon, so agents
keep their fast command-line workflow while people get a quiet, visual place to
understand and manage it.

## What you will notice

- **A calm menu-bar status.** One icon tells you whether Plug is healthy,
  degraded, or down. Open it for the server list, connected clients, recent
  activity, and auth issues; healthy details stay visually quiet.
- **Server management without editing files.** Enable, disable, restart, add,
  remove, and reauthorize servers from the app. Every change still goes through
  the daemon and is saved atomically, so the GUI never becomes a second source
  of truth.
- **A real answer to “what is using Plug?”** The Clients view shows connected
  apps and sessions, while the redacted Activity view shows which client called
  which server, how long it took, and whether it worked.
- **Far fewer lifecycle surprises.** Plug.app owns the daemon through macOS's
  login-service system. It can adopt an older CLI installation, start from the
  correct login session, and tell you plainly when a restart is needed after an
  update. Quitting the app leaves the daemon running.
- **Native, restrained notifications.** Plug only interrupts you for conditions
  that need action: an upstream needs sign-in or a new remote client has been
  authorized. Repeated events coalesce into one notification.

## For agents and MCP clients

The CLI remains first-class and the wire behavior stays dual-era. Legacy clients
continue to work unchanged. Modern peers can use the MCP `2026-07-28` lifecycle
when their direction is enabled, while each upstream still chooses `legacy`,
`auto`, or `modern` independently.

This release closes the active official modern server suite at **22 passed,
0 failed**. In practical terms, modern discovery, tools, completion, mixed
content, progress, SSE streams, resources and templates, prompts, and DNS
rebinding protection now work together through Plug. The suite is still an
official prerelease, so the gates remain explicit rather than pretending the
whole ecosystem has already migrated.

Two subtle fixes matter for real agents: progress updates no longer disappear
between the downstream and upstream request, and a resource URI created from an
advertised template now reaches the server that advertised it.

## Installation and updates

The recommended Mac install is the notarized Plug `.dmg`: drag Plug to
Applications and open it once. The app contains the same universal `plug` binary
as the CLI release, so a new user does not have to install a second package just
to get started.

Plug.app updates itself with Sparkle. Every update is verified twice: by a
Sparkle EdDSA signature and by Apple's Developer ID signature. The app and disk
image are notarized and stapled for offline first launch. Homebrew cask users get
the exact same `.dmg`, with `auto_updates true` so Homebrew and Sparkle do not
fight over ownership.

## Privacy and scope

The app observes Plug; it does not answer elicitation or sampling requests on
behalf of Claude, Cursor, Codex, or another MCP client. Those stay in the client
that received them. The activity feed stores only routing facts and timing, is
bounded, and redacts at capture time—no tool arguments, results, tokens, raw
prompts, or credentials are collected.

The app does not create a second credential store. Upstream OAuth still uses
Plug's existing browser and Keychain flow, and remote-client approval still uses
the existing browser/passkey consent page.
