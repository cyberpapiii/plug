# Status

Open work only. `main` is what exists; `CHANGELOG.md` is what changed. Remove a
line here when it lands. Anything bigger than a line belongs in a GitHub issue.

## In progress

- Slack event delivery (`docs/slack-mcp-events.md`) is merged and running on
  one Mac. Unproven: the dot answering a delivered event in the same thread,
  delivery from public channels the owner has not joined, and unattended
  renewal of the one-day subscription.

## Open

- General events: the core, watching a tool for change, `plug events`, and
  the Events tab are in. Left: relaying events a server emits itself, and
  receivers other than a remote client that signs in. Unproven: a watch
  delivering to a real subscriber end to end. Design and order of work:
  issue #255.
- `plug clients` still lists a remote session Plug does not recognise as
  Unknown. The app names it after its grant.
- Clients missing from the registry, most used first (surveyed 2026-10-03):
  Hermes Agent, Muse Code, Kimi Code, Amp, then Qwen Code and OpenClaw. Check
  each config path against the vendor's docs on the day before writing to it.
  Remote-only, name and icon at most: Claude web and mobile, Grok web
  connectors, Perplexity, Gemini, Le Chat.
- Per-client access is in: a client can be kept from servers and from single
  tools. Left over: the app switches servers only, single tools go through
  `plug clients block`; there is no allow list (`only_servers`); every client
  on the shared bearer token is one client, `remote:shared`; and a changed
  block tells every client to re-read its lists, not only the one it touches.
- Warp and Kiro link but show no app icon: their bundle identifiers were not
  read off an installed copy. A connected Pi, Warp, or Kiro is named from what
  it reports, with no client type behind it.
- Code identifiers still call clients apps (`connectableApps`, `AppLinkRow`,
  `connectedApps`, `busyApps`). Rename when touching those files.
- App polish from daily use: copy, confusing states, recovery gaps. Fix as
  found; no sweep.
- Live downstream OAuth certification is done for Claude Desktop and ChatGPT.
  Codex, Cursor, OpenCode, and a real WebKit platform-passkey ceremony are
  unproven.
- The five signed PlugApp fixture tests run only on a Developer ID host, which
  today means one Mac.

## Later

Wanted, not started, in this order. Listed so none is forgotten. Move a line
up to Open before starting it.

- A guided first run in the app. Import and one-switch linking exist, but
  nothing walks a newcomer through what Plug is and its two sides. Also a
  prompt or skill an agent can follow to set Plug up.
- Block single tools centrally. Asking before a call stays with the client.
- Several accounts for one server without adding the server twice.
- Tools from plain HTTP APIs (OpenAPI, GraphQL) with no MCP server.
- Skills: namespace `skill://` resources per upstream server.
- MCP Apps: confirm the UI capability and `ui://` resources survive
  pass-through in both protocol eras.
- Secret providers such as 1Password for server credentials.
- A code-execution tool surface in place of real tools. Current evidence is
  against it as a default; the opt-in search-then-load mode covers clients with
  hard tool caps.

## Deliberately not doing

- Fully live runtime reconfiguration.
- Modern-era follow-through behind gates (`subscriptions/listen`, mixed-era
  MRTR) until production clients speak MCP 2026.
- Linux tarballs, the shell installer, and the `plug` Homebrew Formula; the
  tap still carries the 0.8.10 formula and nothing updates it. Bring them back
  when someone asks.
