# Slack MCP Events

An opt-in adapter that turns Slack messages into one MCP event,
`slack.ditto_message`, and delivers it to one downstream OAuth client by signed
webhook. It exists so an agent can react to coworker messages without a separate
Slack bot.

It is the only event source in Plug. Events from other upstream servers are not
forwarded, and there is no general event framework; see
[Scope](#scope-what-this-is-not).

Absent `[http.slack_events]` there is no receiver, no worker, and no event
state.

## How it works

```text
Slack Events API -> POST /events/slack (signature verified)
                 -> filter -> durable queue -> signed HTTPS webhook
                 -> the one configured downstream OAuth client
```

The downstream client uses authenticated modern MCP over HTTP: `server/discover`
advertises the capability, `events/list` returns the definition, and
`events/subscribe` / `events/unsubscribe` manage the single subscription.

## What qualifies

- Public channels the authorizing Slack user can see. No local channel list.
- A coworker message containing an exact `<@DOT_USER_ID>` mention starts a
  tracked thread and emits an event.
- A message from the dot itself records its thread without emitting an event,
  which covers owner-started threads the dot has replied in.
- Later human replies in a tracked thread qualify without a mention.
- Thread tracking and source-event deduplication last seven days.

Excluded: the owner's own messages, the dot, other bots, edits and other
subtypes, private channels, and direct messages. Threads that began before
activation are not imported. Slack message text is untrusted data, never
configuration or instructions.

## Configuration

Requirements, all enforced by `plug config check`:

- `http.modern_downstream_enabled = true`
- `http.auth_mode = "oauth"`
- `events:subscribe` listed explicitly in `http.oauth_scopes`, next to the
  scopes already offered
- an enabled upstream server named `slack`

```toml
[http]
oauth_scopes = ["tools:read", "events:subscribe"]

[http.slack_events]
team_id = "T_WORKSPACE"
app_id = "A_SLACK_APP"
owner_user_id = "U_OWNER"
dot_user_id = "U_DOT"
subscriber_client_id = "DOWNSTREAM_OAUTH_CLIENT_ID"
```

`subscriber_client_id` is a client already registered with Plug; list them with
`plug auth clients list`. Existing grants do not gain `events:subscribe`
automatically: the client must be re-approved on Plug's consent page.

Enabling the adapter, or changing any identity in it, needs a restart from
Plug.app. Removing or changing the section in a running daemon immediately
stops the worker from using the old identities.

### Signing secret

Slack signs every request to `/events/slack`. Plug reads that app's signing
secret from the OS Keychain, service `plug`, account
`slack-events-signing:<team_id>:<app_id>`. There is no environment-variable or
file fallback. A missing or malformed secret stops an enabled receiver from
starting.

```sh
plug auth slack-events set --team-id T_WORKSPACE --app-id A_SLACK_APP
plug auth slack-events remove --team-id T_WORKSPACE --app-id A_SLACK_APP
```

`set` reads the 32-character hexadecimal secret from a hidden interactive
prompt and confirms it. It takes no flag, argument, pipe, or environment input,
so a person at the keyboard enters it; an agent must not obtain, reveal, or
enter the secret. Neither command changes configuration or restarts anything.
A running receiver keeps the secret it loaded until its next restart, which is
also when a rotated secret takes effect. `remove` is idempotent.

### Slack app

Point the Slack app's Events API request URL at
`https://<public_base_url host>/events/slack` and subscribe the **user**
installation to `message.channels`, plus `tokens_revoked` and `app_uninstalled`
where Slack offers them. `message.channels` requires `channels:history`.

### Subscribing

The selected client calls `events/subscribe` with exactly
`{"scope":"accessible_public_channels"}` and its callback URL. Channel selectors
and unknown arguments are rejected. Plug verifies the callback with a signed
random challenge before storing the grant.

That client can read the capability and the static `events/list` definition
before it holds `events:subscribe`. Subscribe and unsubscribe then answer HTTP
403 with `WWW-Authenticate: Bearer error="insufficient_scope"`, naming the scope
and the protected-resource metadata, so the client can request incremental
consent. Every other client sees no event capability, catalog, or challenge.

A subscription lasts at most one day. Refresh it before the advertised
`refreshBefore`.

## Durability and failure behavior

State lives under Plug's configuration directory at
`slack-events/<configuration hash>/state.json`: the subscription, webhook
secret, queue, seen event IDs, and tracked threads. The directory is `0700`, the
files `0600`, writes use fsync plus atomic rename, and an exclusive lock forbids
a second writer. Message bodies and callback secrets in that file are sensitive.
A restore or integrity failure stops startup; an uncertain write stops ingress
and delivery until the state is repaired and Plug restarts.

- The queue holds at most 128 events and the store at most 8 MiB. An event that
  does not fit is not acknowledged to Slack.
- Before each delivery Plug rechecks the downstream grant, the live Slack
  identity, the channel's visibility, and that the adapter is still enabled.
- A channel that becomes inaccessible loses its queued events and tracked
  threads. A channel whose visibility cannot be checked is deferred, with
  bounded attempts, without blocking other channels.
- A revoked grant or changed identity clears the whole subscription.
- A signed `tokens_revoked` or `app_uninstalled` clears the subscription and
  queue and latches the source off. Recovery means recreating the event state
  and granting access again; a refresh cannot undo it.
- Rotating the webhook key keeps both signatures valid for five minutes.

Callbacks go to public HTTPS on port 443. Plug validates every resolved address,
pins DNS for the connection, keeps TLS hostname validation, and disables proxies
and redirects. Each event keeps a stable ID and body across up to eight
attempts with fresh signing timestamps and bounded exponential delay. A 2xx
acknowledges it. A 4xx other than 408 or 429 drops it, and 410 also removes the
subscription. Unsubscribe waits for an in-flight send before clearing state.

## Limits

- No replay. Slack retries a failed delivery three times and Plug keeps no
  source cursor, so events sent while Plug is down are lost.
- Delivery is bounded-retry, not exactly-once and not lossless. A callback that
  already accepted an event cannot be recalled by a later unsubscribe.
- Slack delivers only what the authorizing user can see, and caps an app at
  30,000 events per workspace per hour.
- Visibility checks call the Slack API through the `slack` upstream and share
  its rate limits; under load events can be deferred or dropped.
- One Slack user installation, one subscriber, one event type.

## Scope: what this is not

- Not a general Plug event system. The event name, the `slack` upstream, the
  single subscriber, and the subscribe arguments are fixed in code.
- Not pass-through. Plug originates this event; it does not relay MCP events
  from upstream servers.
- Not a Slack bot. It posts nothing and does not replace the dot's own app.
- No private channels, no direct messages, no channel joins, no polling.

## Diagnostics

Logs carry fixed method names, protocol era, whether the caller is the selected
client, and outcome status. They never carry parameters, credentials, account
identifiers, callback details, or message text. JSON-RPC failures are logged as
`jsonrpc_error` without the error body.

## Testing

```sh
cargo test -p plug-core slack_events --lib
```

Runs fake Slack envelopes, fake visibility, fake callbacks, HTTP OAuth fixtures,
independent HMAC vectors, restart persistence, and the rejection, expiry, retry,
revocation, and storage-failure paths. It creates no live grant and sends
nothing to Slack or to any callback.

### Live proof

In one quiet public channel where the owner and a coworker can both see the dot:

1. A coworker mentions the dot. Expect one delivered event.
2. The coworker replies in that thread without a mention. Expect one event.
3. Owner messages, bot messages, and an unrelated thread produce none.
4. Unsubscribe, then have the coworker reply again. Expect no callback.

`owner_proof_until` in `[http.slack_events]` lets the owner run step 1 alone. It
is a Unix timestamp at most one hour ahead; until then an owner message that is
exactly `<@DOT_USER_ID> PLUG_EVENTS_PROOF_20261003` is accepted. Every other
owner message stays excluded and every source authorization check still applies.
Remove the key afterwards.

## Turning it off

Unsubscribe from the client, disable Events on the Slack app, remove
`[http.slack_events]` and the `events:subscribe` scope, restart from Plug.app,
then run `plug auth slack-events remove`.
