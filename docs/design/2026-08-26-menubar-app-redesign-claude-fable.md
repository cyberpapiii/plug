# Plug.app design review and overhaul — 2026-08-26 (claude-fable)

Status: implemented on this branch. Scope is the macOS app's interface only. No
daemon, IPC, installation, or release behaviour changed.

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

### Three sections, not four

**Servers · Connections · Activity**, chosen with a segmented control in the
toolbar rather than a permanent sidebar column. **Auth** was dissolved: an
account belongs to the server that needs it, so signing in happens on the server
row and in the server's detail. Remote grants moved next to the live sessions
they authorize, under one question — who can use Plug — with revoke in reach.

### Words a person would use

`ServerHealth` normalizes the daemon's strings once, at the boundary, and owns
the words: Working, Starting, Sign-in needed, Down, Off. Transports became
"Runs on this Mac" / "Remote server". No interface string says reconciliation,
converge, ownership, daemon, or IPC.

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

## Known gaps, deliberately left

- **Tool names are not shown.** The IPC snapshot carries only `toolCount`, so
  "what can my AI actually do right now?" cannot be answered honestly yet. It
  wants a protocol addition, not an invented list.
- **Server editing** is still add and remove; `UpdateServer` exists in the
  protocol and has no interface.
- **Activity is capped at 200 events** with no paging.
