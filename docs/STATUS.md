# Status

Open work only. `main` is what exists; `CHANGELOG.md` is what changed. Remove a
line here when it lands. Anything bigger than a line belongs in a GitHub issue.

## In progress

- Slack event delivery (`docs/slack-mcp-events.md`) is merged and running on
  one Mac. Unproven: the dot answering a delivered event in the same thread,
  delivery from public channels the owner has not joined, and unattended
  renewal of the one-day subscription.

## Open

- Make the product match the words in `docs/VISION.md`. The app says Apps,
  Remote clients, connected apps, AI app, and "New client connected" for
  agents; the CLI command is `plug clients`; the code calls them Connections;
  the client registry still lists Windsurf.
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

- Identity for connected agents: rename one, give it an icon, and recognise
  unknown and remote ones.
- Per-agent access: choose which servers and tools each agent sees, bound to
  how the agent connects and never to the name it reports.
- General events: forward the events a server emits, add push recipes for
  services beside Slack, and watch a tool on a schedule for changes.
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
