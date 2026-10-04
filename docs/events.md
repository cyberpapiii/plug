# Events

An event tells a client that something happened, so the client does not have
to keep asking. Plug has two sources today:

- **Watch a tool.** Plug calls a tool on a schedule and sends an event when
  its result changes. Works with any server. Described here.
- **Slack messages.** One fixed event, described in
  [slack-mcp-events.md](slack-mcp-events.md).

The overall plan is in
[issue #255](https://github.com/cyberpapiii/plug/issues/255).

## Watch a tool

```toml
[[events.watch]]
name = "inbox"          # the event is named <server>.<name>: gmail.inbox
server = "gmail"
tool = "search_messages" # the tool's own name, without Plug's prefix
arguments = { query = "is:unread" }
every_secs = 300         # default 300, minimum 30
```

`plug reload` picks up added, changed, and removed watches.

The same from the command line, which also checks the tool exists and is
read-only before saving:

```sh
plug events watch gmail search_messages --name inbox --arg query=is:unread
plug events                      # every watch and how it is doing
plug events unwatch gmail.inbox
```

In the app, the Events tab lists the same watches. Watch a Tool asks for a
server, a tool, and how often to check. It offers only tools the server marks
read-only; More options has the event name, the arguments as JSON, and a
switch that shows the other tools.

What Plug does:

- Calls the tool every `every_secs` seconds.
- Remembers the first result and sends nothing for it.
- Sends an event when a later result differs. The event carries the whole new
  result as `data.result`.
- Ignores a failed call. A failure is not a change.
- Skips a result larger than 256 KiB.

A tool whose result changes on every call, for example one that includes the
current time, sends an event on every check. Pick a tool, or arguments, whose
result only changes when something happened.

## Rules

- **Off unless configured.** No watch, no events.
- **Watching must not change anything.** Plug only watches a tool its server
  marks read-only. To watch another tool, add `allow_writes = true` to that
  watch and accept that Plug will call it on every check.
- **Per-client access applies.** A client kept from a server cannot see,
  subscribe to, or receive that server's events. An event already queued for
  it is dropped.
- **Results are untrusted.** The receiving client must treat an event's data
  as content, not as instructions.

## Requirements

Watch events use the same delivery path as the Slack event: a remote client
that speaks MCP `2026-07-28`, signs in with OAuth, and holds the
`events:subscribe` scope. So the config also needs:

```toml
[http]
modern_downstream_enabled = true
auth_mode = "oauth"
oauth_scopes = ["tools:read", "events:subscribe"]
```

The client subscribes with `events/subscribe`, names a webhook URL and a
signing secret, and Plug posts each event there, signed. A subscription lasts
at most one day unless the client refreshes it. Delivery retries up to eight
times with growing waits, then gives up on that event.

## Limits

- Webhook delivery only. Local clients on this Mac cannot receive events yet.
- No replay: an event that happened while nobody was subscribed is gone.
- At most 32 subscriptions.
