# Plug.app design review and overhaul — 2026-08-26 (claude-fable)

Status: implemented on this branch. Round one was the macOS app's interface
only. Round two added operator capability the interface needed — a tool
mutation, tool listing detail, and activity attribution — so it also touches
`plug-core` and the daemon's IPC surface. Installation and release behaviour are
unchanged.

## What the app was

A `MenuBarExtra` in `.menu` style listing five disabled labels and six buttons,
plus a 940×640 `WindowGroup` shaped like a small IDE: a sidebar with four
sections (Servers, Clients, Activity, Auth), a page header per section carrying
decorative metrics, and a stack of five hand-rolled notice banners above the
content.

## Findings

### 1. The menu bar app was not the app

Every capability lived in a window. The menu answered "is it working?" with the
same words the window's footer used, in a different order, and could not fix
anything a person would actually want fixed from there. For background
infrastructure this is backwards: nearly every visit is a glance, and a glance
should not cost a window.

### 2. Problems appeared where they could not be fixed

A server needing OAuth showed as `Sign in required` in **Servers**, with no
button. The button lived in **Auth**, a different section, listing the same
servers again under different words. `downstreamClients` rendered twice — in
**Clients** as "Remote access" and in **Auth** as "Remote clients" — and only
one of those could revoke.

### 3. Five vocabularies for one idea

`Running normally` / `Needs attention` / `Not connected` / `Needs reconciliation`
/ `Updating Plug` / `Everything is running normally` / `Plug installation did not
converge` all described overlapping states, in three different files, with no
shared source. Nothing prevented the menu and the window from disagreeing.

### 4. Protocol vocabulary leaked into the interface

`AuthRequired`, `Failed`, `reconciliation`, `converge`, `ownership`, `daemon`,
`IPC`, `stdio`, `streamable_http` were all shown to a person. Health was
compared as raw strings (`health != "Healthy"`) in four separate files, so
adding a state meant finding every comparison.

### 5. Metrics instead of meaning

Page headers led with `486 Tools` — a number with no action attached, and not
the question anyone arrives with.

### 6. The setup moment fought the user

Adding a server asked for a name and one free-text field, then split it on
whitespace. The way people actually acquire an MCP server is copying a JSON
block out of a README; that path was unsupported, so the block had to be
transcribed by hand into two fields.

### 7. Colour carried meaning alone

`StatusDot` was a coloured circle. Red and green circles are the same circle to
a colour-blind reader and to VoiceOver.

### 8. It polled forever

A 3-second snapshot plus 200 activity events, permanently, whether or not any
surface was visible.

## The overhaul

### One verdict, everywhere

`PlugVerdict.verdict(for:)` is a pure function from `PlugSituation` to a single
`Verdict`: tone, symbol, one sentence, one optional detail, at most two buttons.
The menu bar icon, the popover headline and the window banner all render that
one value, so the app cannot contradict itself. The priority order is the
product decision, and it is pinned by 16 tests:

    blocked setup → permission → runtime stopped → version mismatch
      → one named server problem → several problems → starting → all good

When exactly one thing is wrong it is named and fixed in the same sentence
("Notion needs you to sign in" · **Sign In**). When several are, the headline
counts them and each gets its own row with its own button.

### The popover is the app

`MenuBarExtra` is now `.window` style: verdict, then the problems with their
fixes, then a quiet list of running servers, then connected apps, then a
three-affordance footer. Most visits end here.

### Four sections

**Servers · Tools · Connections · Activity**, chosen with a segmented control in
the toolbar rather than a permanent sidebar column. **Auth** was dissolved: an
account belongs to the server that needs it, so signing in happens on the server
row and in the server's detail. Remote grants moved next to the live sessions
they authorize, under one question — who can use Plug — with revoke in reach.

### Words a person would use

`ServerHealth` normalizes the daemon's strings once, at the boundary, and owns
the words: Running, Starting, Sign-in needed, Down, Off. Transports became
"Runs on this Mac" / "Remote server". No interface string says reconciliation,
converge, ownership, daemon, or IPC.

The verdict strings are plain product copy, not narrative. They state what is
true and, when something is wrong, what it is:

| Situation | String |
| --- | --- |
| Everything healthy | "All servers running" / "2 servers · 7 tools" |
| One server needs an account | "Notion needs sign-in" |
| Sign-in in progress | "Signing in" / "Sign-in is open in the browser." |
| Several problems | "2 servers need attention" |
| Service stopped | "Plug is not running" |
| Update staged | "Restart required to finish update" |

### Shape, not colour

Each state has its own SF Symbol and its own accessibility label; colour is
redundant reinforcement. The menu bar icon changes silhouette between working,
busy, attention and stopped — verified by test.

### Paste anything

Add Server takes one text area and understands the `mcpServers` block, a bare
entry, a URL, or a shell command with environment prefixes — lifting `env`,
`args` and a bearer header, guessing the name from the package rather than the
runner, then showing what it understood before saving. 13 tests cover the shapes
that arrive in practice, including truncated JSON, which now explains itself.

### Intents

Views name a `PlugIntent`; `PlugIntentRunner` is the only place an intent
becomes work. Adding an action means adding one case, not wiring a closure
through four view layers.

### Polls only when watched

Surfaces call `setWatching(_:)` on appear and disappear: 2s while someone is
looking, 30s when nobody is. The manual Refresh button is gone — the app is
live — but ⌘R still works, as a command rather than a piece of chrome.

## What did not change

Installation, adoption, launchd ownership, IPC, notifications, Sparkle updates,
and every service and coordinator behind them. `AppModel`'s existing surface is
intact and its tests are untouched.

## Round two: the same power as the CLI

The first pass made the app calm. It did not make it capable. Round two closed
the gap between what a person can do in the terminal and what they can do in the
interface, keeping the same voice.

### Tools are named, and can be switched off one at a time

The daemon already listed tools over IPC; Swift simply never modelled them.
`ToolCatalog` groups the merged catalog by server, strips the `server__` prefix
for display, and searches across name, server and description — so typing
"figma" shows what Figma can do rather than nothing.

Switching a tool off is a new operator mutation, `SetToolEnabled`, carried by
IPC version 5 behind the `tool_mutation` capability and written through the
existing atomic config path. Enabling refuses to widen a covering wildcard: the
config format cannot express "this pattern except one tool", so a tool hidden by
`figma__*` shows as Off with the pattern named, instead of the app quietly
switching on 118 tools nobody asked for. Older daemons simply do not advertise
the capability, and the switches render read-only.

### Servers can be edited, not only added and removed

`EditServerView` prefills the real fields — command and arguments, or URL and
token, plus environment — validates through `ValidateServer`, then writes with
`UpdateServer`, which had been in the protocol without an interface.

### Activity says which tool, from which app, in which session

`ActivityEvent` gained `tool`, `client_type` and `client_label`, all optional
and default-tolerant, filled at the single emit site in `dispatch_tools_call`.
The client id is minted per `plug connect` process, so it is the per-window
session discriminator; a row now reads as the tool name over "Claude Code ·
session 8f21 · figma". The labels are copied onto the event so attribution
survives the session disconnecting.

### Apps can be wired up from the app

Connections gained "Apps on this Mac": each detected client with a switch that
runs `plug link` / `plug unlink`. That wiring lives in each client's own
configuration file, which the daemon does not own, so the app shells the bundled
binary — the same pattern `ClientRepairService` and `AuthFlowService` use.

## Round three: the ordinary things, and pictures instead of sentences

Two more review notes: the app was missing the basics any Mac app has, and it
leaned on words where a picture would land faster.

### Settings is a real place

Three tabs, each answering one question.

- **General** — open at login, whether Plug is allowed to interrupt with a
  notification, and whether it looks for updates on its own. The notification
  switch is new: `NotificationService` now reads a preference before posting,
  defaulting to on.
- **Service** — whether the background service is running, Restart, Reload
  settings, and a checkup. Restart goes through `DaemonServiceManager.restart()`,
  the same path installation uses, so nothing in the app ever signals the
  process itself. Reload is the existing `Reload` IPC request, which the Swift
  protocol did not model until now; its report comes back as `ReloadSummary` and
  says what moved.
- **About** — version, servers on, tools available, Check for Updates, and Quit.

The checkup runs `plug doctor --output json` and shows the same checks the
terminal prints, one row each, trouble first, with each check's identifier
turned into a title a person can read (`config_permissions` becomes "Settings
file is private"). A checkup that finds problems exits non-zero; that is the
answer, not a failure, so the exit status is deliberately ignored.

### Quit and Settings are visible

Both were inside an ellipsis menu, which is where controls go to be lost. They
are now icon buttons in the popover footer, with tooltips and accessibility
labels carrying the words. The window has its own Settings button, because Plug
is an accessory app and has no menu bar of its own.

Quitting says what it costs: the servers keep running, and connected apps keep
working. Only the menu bar icon goes away.

### Real app icons

`AppIcons` resolves an app's own icon from this Mac — by bundle identifier for
the apps Plug knows, then by name under `/Applications` — and falls back to a
symbol that still says what kind of thing it is (a terminal for command line
tools, an editor glyph for editors). Connections and Activity now show Claude's
icon, Cursor's icon, VS Code's icon. A row is recognized before it is read.

### Icons carry state everywhere else

- A server's second line is prefixed by where it runs: a screen for this Mac, a
  globe for a remote server, a warning triangle when the line is an error, a
  slashed circle when it is switched off.
- A tool held off by a wildcard shows a padlock, not just the word "Off".
- Tool group headers carry a filled box, hollow when the whole server is off.
- The section picker shows icon and word together.
- Every check in the checkup has a green tick, an orange triangle, or a red
  cross, with the outcome also spoken in its accessibility label.

Colour is never the only carrier: every glyph is a distinct shape, and every one
has words behind it for VoiceOver and for anyone who does not read colour.

## Known gaps, deliberately left

- **Importing servers from other apps** (`plug import`) and **signing out of a
  server** (`plug auth logout`) are still terminal-only. Doctor, reload, and the
  config path arrived in Settings; these two did not.
- **Activity is capped at 200 events** with no paging.
