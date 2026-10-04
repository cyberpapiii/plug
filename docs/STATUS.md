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
- Clients missing from the registry (surveyed 2026-10-04): Muse Code, Amp,
  OpenClaw, LM Studio. Check each config path against the vendor's docs on
  the day before writing to it; on 2026-10-04 Amp's manual did not show its
  settings path. Amp is said to keep its servers under the key
  `amp.mcpServers`, OpenClaw under `mcp.servers` in a JSON5 file; linking,
  repair, and doctor read neither.
- Remote clients are named and pictured by guesswork: the app matches
  Gemini, Perplexity, Le Chat, and Poke on the name a client reports, and
  nobody has seen what each one reports. Correct the match when one connects.
- Per-client access is in: a client can be kept from servers and from single
  tools. Left over: the app switches servers only, single tools go through
  `plug clients block`; there is no allow list (`only_servers`); every client
  on the shared bearer token is one client, `remote:shared`; and a changed
  block tells every client to re-read its lists, not only the one it touches.
- Plug needs a real app icon: minimal, distinct, and legible small, because
  it shows in the Dock, the menu bar, and inside every client Plug connects
  to. The current one is a placeholder.
- Text-message agents: Poke takes a custom MCP server and can reach Plug as
  a remote client. Instinct and Tomo document no way to add one (2026-10-04).
- The app needs one audit and redesign, whole: every flow, the menu bar,
  and the CLI's wording, held to the grandma test. Known complaints:
  Settings is a separate window and should live in the main one; layout
  bugs and rough edges throughout; the three surfaces do not feel like one
  product. Audit and write the findings before changing anything.
- Order agreed 2026-10-04: clients and their icons (done), secret stores
  (#264), skills pass-through (#262), the app redesign, the app icon, then a
  release. Skills is wanted now, small and clean, not held for a server
  that uses it.
- CI has no job timeout. A hung `Test (Plug.app)` ran 65 minutes on #269
  before it was cancelled by hand.
- A connected Pi, Warp, or Kiro is named from what it reports, with no client
  type behind it. Warp's and Kiro's icons use identifiers read off Homebrew's
  casks, not off an installed copy.
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

- GraphQL APIs as servers. Decided 2026-10-04: not now. Build the small
  built-in form described on #263 on the day a GraphQL API with no MCP
  server is needed. OpenAPI servers are done: CLI and app, bearer token or
  API key, operations chosen in either.
- Skills: carry the skills extension through Plug, with the server's name in
  each skill URI. Waits for a server and a client that use it (#262).
- Secret stores (#264). Done: `keychain:<name>` references and
  `plug secret set`. Next, in order: the app and `plug server add` put a
  pasted key in the Keychain by default and `plug doctor` offers to move
  plaintext keys; the `.env` file as a store read only by the service; any
  other store as a command in config, then `op://`.
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
