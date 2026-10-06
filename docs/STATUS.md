# Status

Open work only. `main` is what exists; `CHANGELOG.md` is what changed. Remove a
line here when it lands. Anything bigger than a line belongs in a GitHub issue.

## In progress

- Slack event delivery (`docs/slack-mcp-events.md`) is merged and running on
  one Mac. Unproven: the dot answering a delivered event in the same thread,
  delivery from public channels the owner has not joined, and unattended
  renewal of the one-day subscription.

## Next, in order

Agreed with the owner on 2026-10-05. No release until the first four are in.
Remove a line when it lands.

1. Paper cuts. With the sidebar collapsed, the window's title and count
   overlap the first column. Add others here as the owner reports them.
2. Client audit. Remove clients that are stale or gone (Windsurf is one;
   find where the app still shows it). Tell GrokBot from Cursor: the owner
   has GrokBot make a few tool calls, and its id is read off them. Name the
   connections that show as `python3` (Hermes Agent).
3. Naming layer, what is left of the check-up: an agent sees the account
   label the owner typed, not the address that is signed in, and renames
   are still config-only. Issue #301.
4. Widening: an allow list per client, stored like a tool block is (by
   server and the server's own tool name), the shared bearer token split
   into separate clients, and relaying events a server emits itself (#255).
5. Single servers served on their own, beside the one Plug endpoint, so a
   client can mix its own connectors with only the servers it lacks. Comes
   out of the allow list. Issue #304.
6. Plug into Plug: several Plugs feeding one, each remote server shown as
   its own server and marked with its machine. Design first. Issue #302.

Unproven, to watch working along the way: an event watch delivering to a
real subscriber, Slack renewing its subscription unattended, and one of
Amp, OpenClaw, LM Studio, or Muse Code on a real install.

## Open

- General events: the core, watching a tool for change, `plug events`, and
  the Events tab are in. Left: relaying events a server emits itself, and
  receivers other than a remote client that signs in. Unproven: a watch
  delivering to a real subscriber end to end. Design and order of work:
  issue #255.
- Amp, OpenClaw, LM Studio, and Muse Code are linked from their makers'
  documentation as read on 2026-10-05; none was installed to try. Correct
  the path or the entry when one is. Amp and OpenClaw allow comments in
  their settings file, and Plug refuses to write a file that has them.
- Remote clients are named and pictured by guesswork: the app matches
  Gemini, Perplexity, Le Chat, and Poke on the name a client reports, and
  nobody has seen what each one reports. Correct the match when one connects.
- Per-client access is in: a client can be kept from servers and from single
  tools. Left over: there is no allow list (`only_servers`); every client
  on the shared bearer token is one client, `remote:shared`; and a changed
  block tells every client to re-read its lists, not only the one it touches.
- `health::tests::a_server_that_stays_down_backs_off_and_stays_quiet` failed
  once under a full run on 2026-10-06 and passed on every rerun. Look at its
  timing if it fails again.
- `a_tool_turned_off_for_everyone_is_neither_listed_nor_callable` listed no
  tools once under a full run on 2026-10-06 and passed on every rerun.
- A session is checked against its sign-in when it is used or sent
  something, not on a clock. One whose sign-in has run out and that hears
  nothing stays open, unusable, until it goes idle.
- Clients lists a client that came and went from its last call in Activity,
  which holds 500 calls and starts empty when Plug starts. A client kept
  from tools this way keeps its blocks; only its row is gone until it calls.
- `scripts/test-app.sh` failed three `UnifiedReconciliationFixtureTests` with
  `invalidSignature` on 2026-10-06: the daemon in the test build had been
  replaced after Xcode signed the bundle, and Xcode did not sign again.
  Moving `PlugApp/.build/tests/Build/Products/Debug/Plug.app` aside fixed
  it. Find what leaves the bundle unsigned if it happens again.
- Text-message agents: Poke takes a custom MCP server and can reach Plug as
  a remote client. Instinct and Tomo document no way to add one (2026-10-04).
- A connected Pi, Warp, or Kiro is named from what it reports, with no client
  type behind it. Warp's and Kiro's icons use identifiers read off Homebrew's
  casks, not off an installed copy.
- Code identifiers still call clients apps (`connectableApps`, `AppLinkRow`,
  `connectedApps`, `busyApps`). Rename when touching those files.
- The window was rebuilt on the standard Mac layout (sidebar, list and
  detail, Settings window, menus) after the owner found #276 inconsistent.
  Each screen was checked from the app's own drawing of itself in light and
  dark. Not seen: the menu bar panel, the sidebar's glass, and Events with a
  watch in it. Look at those once and file what reads wrong.
- The app icon (a white power symbol on blue) was chosen without the owner
  seeing it in the Dock. Confirm it, or swap the files in `docs/assets/` and
  the app's icon set.
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
- Skills (#262). Done: `skill://` resources carry their server's name
  everywhere a URI crosses Plug. Left, when a client Plug's owner uses calls
  them: `skills/list` and `skills/get` on the 2026-07-28 path with the
  extension's capability, then `resources/directory/read`.
- Secret stores (#264). Done: the Keychain, the `.env` file, 1Password, and
  any command as stores; `plug secret set`, typed keys going to the Keychain
  or the `.env` file as the server form chooses, `plug secret move`. Left:
  `$NAME` expansion from `.env` still works beside `file:<name>` and could
  be retired.
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
