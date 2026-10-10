# The settings file

Plug keeps its settings in one file:

```
~/Library/Application Support/plug/config.toml
```

The app and the `plug` command write this file for you, so most people never
open it. `plug config` opens it in your editor, `plug config check` says
whether it is valid, and Settings, Files, Reload in the app applies a change
without a restart.

## Servers

A server is a command on this Mac or a remote service with an address.

```toml
# A command on this Mac
[servers.github]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]
env = { GITHUB_TOKEN = "$GITHUB_TOKEN" }

# A remote service you sign in to
[servers.notion]
transport = "http"
url = "https://mcp.notion.com/mcp"
auth = "oauth"
```

`$NAME` in a value is replaced by that environment variable when Plug starts.
To keep a key out of this file, run `plug secret move` and Plug stores it in
the Keychain.

Per server you can also set:

| Key | Default | What it does |
|---|---|---|
| `enabled` | `true` | Switch the server off without removing it |
| `timeout_secs` | `30` | How long Plug waits for the server to answer when connecting |
| `call_timeout_secs` | `300` | How long one tool call may take |
| `max_concurrent` | `1` | How many calls run on the server at once |
| `health_check_interval_secs` | `60` | How often Plug checks the server is alive |
| `oauth_scopes` | none | Scopes to ask for at sign-in |
| `protocol` | `legacy` | `legacy`, `auto`, or `modern`. See [guides/mcp-2026-dual-era.md](guides/mcp-2026-dual-era.md) |

## Tool names

Every tool is named `<Server>__<tool>`, for example `Slack__channels_list`,
so two servers can never collide. A second account on a server is the same
server under another name: `plug server add-account slack work` adds
`slack-work`.

A large server can be split into groups, and a tool can be renamed:

```toml
[servers.workspace.tool_renames]
search_docs = "get_doc_search_results"

[[servers.workspace.tool_groups]]
prefix = "Gmail"
contains = ["gmail"]
strip = ["gmail"]
```

## Clients with tool limits

Clients see every tool by default. For a client that cannot hold many tools,
Plug can show one search tool first and load real tools as the client finds
them:

```toml
[lazy_tools]
mode = "auto"        # auto, standard, native, bridge

[lazy_tools.clients]
opencode = "bridge"  # search first, then call the real tool
```

`plug clients -v` shows which mode each client got and why.

Which servers and tools a client may use is set on the client's page in the
app, or with `plug clients block`, `plug clients unblock`, and
`plug clients only`.

## Remote access

```toml
[http]
bind_address = "127.0.0.1"
port = 3282
```

Reaching Plug from outside this Mac needs a public address and sign-in. See
[OPERATOR-GUIDE.md](OPERATOR-GUIDE.md).

## Events

See [events.md](events.md).
