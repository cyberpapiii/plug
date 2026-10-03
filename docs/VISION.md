# Vision

## What Plug is

Plug is one place on your Mac that holds every tool you have and gives it to
every agent you use, wherever that agent runs.

Some tools only work on your Mac: Messages, Contacts, location, anything that
reads local files or local apps. Others are remote services. Plug runs them
all from one always-on Mac, keeps them healthy, signs you in once, and streams
them to agents on the Mac or in the cloud. You configure once and you own the
configuration. No agent vendor can lock you in, because the tools live with
you, not with them.

Plug is an MCP gateway. It is a personal tool, one daemon, operationally
boring, and it must never be the fragile part of the chain.

## Three jobs

1. **Give tools to agents.** Real tools, passed through unchanged, with health
   monitoring, recovery, sign-in, and a record of what happened. This is the
   core, and it is mature.
2. **Tell agents when something happens.** Events from a server that emits
   them, from a push recipe for services like Slack, or from watching a tool
   for change. Slack is the first source; the feature is general.
3. **Be understood without help.** A newcomer, or their agent, can install
   Plug, see its two sides, connect a server and an agent, and know what
   happened, with no documentation open.

## Words

Five nouns. Humans and agents use the same ones, and they are the protocol's
words wherever an agent can see them.

| Word | Meaning |
|---|---|
| Server | A program that provides tools. On this Mac or remote. |
| Tool | One thing a server can do. |
| Agent | A program that uses tools: Claude, ChatGPT, Codex, Cursor. On this Mac or remote. |
| Event | Something that happened on a server, or a change Plug noticed by watching a tool, delivered to the agents that subscribed. |
| Activity | The record of tool calls and event deliveries. |

Verbs: add a server, connect an agent, block a tool, subscribe to an event.
Access is a set of switches on an agent's page, never a new noun. When one
server needs several sign-ins, the word is *account*.

Avoid: *app* (means a server in ChatGPT and Gemini), *client* (protocol term
in code and a hidden CLI alias, nowhere a person reads), *connection*,
*connector*, *integration*, *source*, *plugin* (each means something else in
some product), *trigger* (one word for events), *logs* (the files under
`~/Library/Logs/plug`), and *multiplexer* (nobody else says it; say MCP
gateway).

## Principles

1. **Pass through first.** An agent sees a server's real tools with their real
   names, schemas, and annotations. Plug adds beside them, never in place of
   them. The search-then-load mode exists for agents with hard tool caps and
   is opt-in.
2. **Plug blocks, the agent asks.** Plug can hide a tool or refuse a call.
   Asking the human before a call is the agent's job, because only the agent
   has the conversation.
3. **One place, many agents.** Per-agent access is bound to how the agent
   connects (its socket, its OAuth grant), never to the name it reports. It
   keeps tool lists tidy for agents on this Mac and is a real boundary for
   remote ones.
4. **Grandma-simple power.** Advanced options exist and are reached by
   guidance: a first run, a wizard for the hard steps, plain copy, icons over
   words. When a flow is inherently complex, Plug guides it rather than
   hiding it.
5. **Reliability is the product.** One bad server never poisons the rest.
   Recovery is automatic. Errors say what to do. Shutdown is clean.
6. **App first, CLI underneath.** Everyday jobs live in the app. The CLI and
   `--output json` are for agents and scripts; they talk to the same daemon
   and never need the window open. Neither surface hides state the other
   shows.
7. **Current with the protocol.** Serve both MCP eras while real clients need
   both. Adopt a new capability when a real client can use it.

## Not doing

- Teams, organizations, roles, billing, or a hosted service.
- Docker, a database, or a required cloud account.
- A second UI beside the app.
- Running tools through generated code instead of calling them.
- Becoming an installer or package manager for upstream servers.

## Done looks like

- A newcomer finishes the first run and understands the two sides.
- Every agent on the Agents page has a name and an icon, including unknown and
  remote ones.
- Each agent can be given a subset of servers and tools.
- Any server can be an event source, and any subscribed agent receives it.
- An agent can set Plug up for its human from a prompt or skill.
- Docs describe the product as it exists.
