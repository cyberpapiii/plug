# Plug Operator Guide

This guide is for running Plug as shared infrastructure: a local daemon for daily agent work, or a remote MCP gateway for trusted clients.

## Runtime Model

Plug has one configured upstream set and many downstream clients.

- `plug start` starts the shared daemon, IPC listener, and HTTP server.
- `plug connect` is the stdio adapter most local clients use.
- `/mcp` is the Streamable HTTP endpoint for HTTP-capable clients.
- `plug status`, `plug clients`, `plug servers`, `plug tools`, and `plug doctor` are the primary operator surfaces.

Useful files:

- Config: `~/.config/plug/config.toml`
- macOS logs: `~/Library/Logs/plug/`
- macOS runtime state: `~/Library/Application Support/plug/`
- Linux logs/state: `~/.local/state/plug/`

Prefer `--output json` for automation:

```sh
plug status --output json
plug clients --output json
plug servers --output json
plug tools --output json
```

## HTTP, TLS, And Exposure

Loopback-only local use can keep the defaults:

```toml
[http]
bind_address = "127.0.0.1"
port = 3282
auth_mode = "auto"
```

For remote clients, set a public URL and TLS material:

```toml
[http]
bind_address = "0.0.0.0"
port = 3282
public_base_url = "https://plug.example.com"
auth_mode = "oauth"
tls_cert_path = "/etc/plug/tls/fullchain.pem"
tls_key_path = "/etc/plug/tls/privkey.pem"
allowed_origins = ["https://claude.ai"]
```

Rules enforced by Plug:

- Non-loopback binds require `tls_cert_path` and `tls_key_path`.
- `auth_mode = "oauth"` requires `public_base_url`.
- Cert and key paths must be set together.
- Private keys must not be group/world readable on Unix.

Put Plug behind a reverse proxy only if the proxy preserves normal HTTP request headers and forwards the public `/mcp` URL consistently. `public_base_url` must be the URL clients actually use.

## Downstream Auth

Downstream auth protects clients connecting to Plug.

Modes:

- `auto`: default. Loopback is unauthenticated; non-loopback uses bearer auth.
- `none`: unauthenticated. Use only on loopback or tightly controlled private networks.
- `bearer`: protects `/mcp` with a bearer token.
- `oauth`: local OAuth authorization server with PKCE and token refresh support.

Bearer mode:

```toml
[http]
auth_mode = "bearer"
public_base_url = "https://plug.example.com"
```

OAuth mode:

```toml
[http]
auth_mode = "oauth"
public_base_url = "https://plug.example.com"
oauth_client_id = "plug-client"
oauth_client_secret = "$PLUG_DOWNSTREAM_CLIENT_SECRET"
oauth_scopes = ["mcp:read", "mcp:write"]
```

OAuth discovery endpoints:

- `/.well-known/oauth-authorization-server`
- `/.well-known/oauth-protected-resource`
- `/.well-known/oauth-protected-resource/mcp`

Operator endpoints use a separate `x-plug-operator-token` and are not protected by downstream MCP bearer/OAuth tokens. Treat the operator token as an administrative secret.

## Upstream OAuth

Upstream OAuth protects Plug when it connects to remote MCP servers.

Example:

```toml
[servers.remote_docs]
transport = "http"
url = "https://docs.example.com/mcp"
auth = "oauth"
oauth_client_id = "plug"
oauth_scopes = ["mcp:read", "mcp:write"]
```

Login and inspect state:

```sh
plug auth login --server remote_docs
plug auth status
```

Plug stores reusable OAuth credentials through the configured credential store and refreshes tokens in the background when possible. If a server registration cannot be reused safely, rerun `plug auth login --server <name>` or configure a stable `oauth_client_id`.

## Observability

Start with:

```sh
plug status
plug doctor
plug clients
plug servers
plug tools
```

For logs:

```sh
RUST_LOG=plug=debug,plug_core=debug plug start
```

HTTP tracing:

- Plug accepts W3C `traceparent` and `x-plug-trace-id` on downstream HTTP requests.
- Plug propagates trace IDs across router calls, upstream retries, reconnects, and auth-refresh logs.
- Plug validates present SEP-2243 `Mcp-Method` and `Mcp-Name` headers against the JSON-RPC body.
- Plug emits `Mcp-Method` and `Mcp-Name` when proxying HTTP/SSE upstream requests.

Operator inventory:

- `plug tools --output json` includes source metadata, trust boundary, upstream-declared annotations, Plug-inferred annotations, and effective annotations.
- `plug servers --output json` includes configured transport/auth/trust metadata without serializing secrets.
- `/_plug/live-sessions` exposes active session inventory for local operator tooling and requires `x-plug-operator-token`.

## Stdio Upstream Sandboxing

Plug executes configured stdio commands. Only add upstream commands you trust, or enable sandboxing for third-party/local-risk servers.

Sandboxing is opt-in per stdio server:

```toml
[servers.filesystem]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/Users/rob/projects"]

[servers.filesystem.sandbox]
enabled = true
allow_network = false
allow_read = ["/Users/rob/projects"]
allow_write = ["/Users/rob/projects/.cache"]
```

On macOS, Plug uses `/usr/bin/sandbox-exec` with a generated deny-by-default profile, or a custom profile:

```toml
[servers.custom.sandbox]
enabled = true
profile_path = "/Users/rob/.config/plug/sandbox/custom.sb"
```

Current limits:

- Sandboxing is implemented for stdio transports only.
- macOS enforcement is implemented; non-macOS sandbox config currently fails fast.
- CPU/memory/process limits are not implemented yet.

## Release Operations

Current distribution names:

- GitHub repo: `cyberpapiii/plug`
- Homebrew tap: `cyberpapiii/tap/plug`
- crates.io package: `plug-mcp`
- Installed binary: `plug`

Release checks before publishing:

```sh
cargo test --workspace -- --test-threads=1
cargo clippy --workspace -- -D warnings
cargo deny check advisories
dist plan --no-local-paths
dist build --artifacts=global
dist build --artifacts=local --target aarch64-apple-darwin
```

Publish order matters for crates.io: publish `plug-core` before `plug-mcp`, because the CLI package depends on the library package by version. The public install command is `cargo install plug-mcp --locked`; use `cargo install --git https://github.com/cyberpapiii/plug plug-mcp --locked` only when validating unreleased `main` builds.
