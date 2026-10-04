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
  Hermes Agent, Muse Code, Amp, then OpenClaw. Check each config path against
  the vendor's docs on the day before writing to it. Two are known and need
  more than a new row: Amp keeps its servers under the key `amp.mcpServers` in
  `~/.config/amp/settings.json`, and Hermes Agent under `mcp_servers` in
  `~/.hermes/config.yaml`; linking, repair, and doctor look only for
  `mcpServers`, `context_servers`, and `servers`.
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

- Tools from plain HTTP APIs: an OpenAPI document as a server, and a recipe
  for GraphQL (#263).
- Skills: carry the skills extension through Plug, with the server's name in
  each skill URI. Waits for a server and a client that use it (#262).
- Secret providers: server credentials from a 1Password mount, read by the
  service only and never hanging it (#264).
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
