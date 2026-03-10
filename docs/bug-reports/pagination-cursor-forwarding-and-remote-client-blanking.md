# Bug: Claude Desktop UI blanks out and loses tools when connecting via remote HTTP

**Severity:** Critical — renders remote MCP connections unusable
**Affects:** All remote clients via `plug serve` (Streamable HTTP), all IPC clients via `plug connect` (stdio/daemon)
**Discovered:** 2026-03-10
**Fixed in:** `fix/claude-remote-protocol-version` branch (commit c66f486)

## Summary

Claude Desktop connecting to plug via Cloudflare tunnel (Streamable HTTP) would show tools initially, then blank out the entire UI after the first tool use. Local `plug connect` clients were missing tools past position 100 in the merged list. Three interacting bugs plus one client-side limitation combined to make remote connections unusable and local connections incomplete.

## Environment

- plug: built from source, `fix/claude-remote-protocol-version` branch
- Claude Desktop: remote MCP connector via Cloudflare quick tunnel
- Claude Code: local stdio via `plug connect` → daemon IPC
- 170+ merged tools from 10+ upstream MCP servers
- macOS 15.4, Apple Silicon

## Root Causes

### Bug 1: Daemon IPC ignored pagination cursors (daemon.rs) — CRITICAL

**Files:** `plug/src/daemon.rs`

The daemon IPC request dispatcher for `resources/list`, `resources/templates/list`, and `prompts/list` all passed `None` instead of forwarding the cursor from the client's request params.

```rust
// Before (broken):
"resources/list" => {
    let result = tool_router.list_resources_page(None);  // always page 1
}

// After (fixed):
"resources/list" => {
    let request = params
        .and_then(|p| serde_json::from_value::<PaginatedRequestParams>(p.clone()).ok());
    let result = tool_router.list_resources_page(request);  // forwards cursor
}
```

With 100+ resources and PAGE_SIZE=100, this created an infinite pagination loop. The client would receive `nextCursor: "100"`, request page 2, but always get page 1 back (with another `nextCursor: "100"`). Claude Desktop's MCP implementation processed ~1,762 requests/second generating ~2.5GB of JSON through stdio, causing the UI to blank out entirely.

The same pattern existed for `tools/list` in the daemon handler — it called `list_tools_for_client` (which returns all tools unpaged) instead of `list_tools_page_for_client`.

### Bug 2: IPC proxy discarded tools/list pagination params (ipc_proxy.rs) — HIGH

**File:** `plug/src/ipc_proxy.rs`

The `ServerHandler` impl's `list_tools()` method accepted `request: Option<PaginatedRequestParams>` but discarded it, always sending `params: None` to the daemon:

```rust
// Before (broken):
fn list_tools(&self, _request: Option<PaginatedRequestParams>, ...) {
    // ...
    params: None,  // pagination params thrown away
}

// After (fixed):
fn list_tools(&self, request: Option<PaginatedRequestParams>, ...) {
    let params = request
        .map(serde_json::to_value)
        .transpose()
        .map_err(|e| McpError::internal_error(e.to_string(), None))?;
    // ...
    params: params.clone(),  // forwards pagination
}
```

This meant any `plug connect` client with tools past position 100 could never reach them.

### Bug 3: PAGE_SIZE too small for real-world tool counts (proxy/mod.rs) — HIGH

**File:** `plug-core/src/proxy/mod.rs`

`PAGE_SIZE` was set to 100. With 170+ tools from 10+ servers, tools past position 100 were on page 2. This interacted with Bug 2 (IPC proxy couldn't paginate) and the client limitation below to make those tools unreachable.

```rust
// Before:
const PAGE_SIZE: usize = 100;

// After:
const PAGE_SIZE: usize = 500;
```

Changed to 500 to fit all current tools in a single page, eliminating pagination edge cases entirely.

### Client Limitation: Claude Desktop remote connector doesn't follow MCP pagination

Claude Desktop's remote MCP connector sends `tools/list` once, receives the first page, and ignores `nextCursor`. It also does not open an SSE stream (GET /mcp), so it never receives `tools/list_changed` notifications.

This is a client-side limitation, not a plug bug. Plug's pagination implementation is correct per the MCP spec. The PAGE_SIZE increase to 500 works around this by ensuring all tools fit in one page.

## Additional Issues Discovered During Investigation

### Figma server OAuth crash loop

The Figma upstream server (`npx mcp-remote https://mcp.figma.com/mcp`) was returning HTTP 403 on OAuth, causing plug to enter a connect-fail-retry loop. Fixed by disabling the server in config (`enabled = false`).

### iMessage Max session exhaustion and degradation

Multiple plug instances connecting simultaneously exhausted iMessage Max's session limit (HTTP 503 "Too many active sessions"). Additionally, a Swift continuation leak in iMessage Max causes gradual degradation over time, eventually producing HTTP 406 responses. Periodic restarts of iMessage Max are required as a workaround.

**Fix:** Restart via `launchctl stop local.imessage-max` (it auto-restarts via launchd).
**Upstream issue:** The Swift continuation leak needs to be fixed in iMessage Max itself.

### Auto-spawned daemon race condition

When `plug connect` detects no daemon on the Unix socket, it auto-spawns one. Multiple `plug connect` processes starting simultaneously (e.g., multiple Claude Code windows) each spawn their own daemon, creating 3-4 competing processes fighting for the socket. Only one wins; the rest hold stale connections.

**Workaround:** Kill extra daemon processes. The launchd-managed daemon (`com.plug.daemon`) will hold the socket.
**Pattern:** Just `kill <pid>` — launchd's `KeepAlive` restarts automatically. Do NOT run `launchctl start` afterward or you'll create duplicates.

### "Anthropic Proxy: Invalid content from server"

When using Claude Desktop via the remote HTTP path, Anthropic's proxy sometimes rejects tool responses with "Invalid content from server". This appears to be an Anthropic-side infrastructure issue, not a plug bug. Observed specifically with Slack tool responses containing large payloads.

### Binary killed by macOS (exit 137)

Manually copying `target/release/plug` to `~/.local/bin/plug` produced a binary that macOS killed on execution. The correct install method is:

```bash
cargo install --path plug --force
```

This installs to `~/.cargo/bin/plug` (symlinked from `~/.local/bin/plug`) and properly signs/notarizes the binary.

## Infrastructure Notes

### launchd services for persistent operation

Two launchd services keep plug running as a background service:

| Service | Command | Purpose | Plist |
|---------|---------|---------|-------|
| `com.plug.serve` | `plug serve` | HTTP server on 127.0.0.1:3282 (for remote clients) | `~/Library/LaunchAgents/com.plug.serve.plist` |
| `com.plug.daemon` | `plug serve --daemon` | Unix socket IPC (for local `plug connect` clients) | `~/Library/LaunchAgents/com.plug.daemon.plist` |

Both have `KeepAlive` (restart on crash) and `RunAtLoad` (start at login). Logs go to `~/Library/Logs/plug/`.

**Management:**
- Kill to restart: `kill <pid>` — launchd handles restart automatically
- View logs: `tail -f ~/Library/Logs/plug/serve-stderr.log`
- These are SEPARATE modes, not layered. `plug serve` runs its own engine; `plug serve --daemon` runs the daemon engine.

### Cloudflare tunnel for remote access

Quick tunnel (ephemeral URL):
```bash
cloudflared tunnel --protocol http2 --url http://127.0.0.1:3282 --no-autoupdate
```

The quick tunnel URL changes on every restart. For a stable URL, configure a named tunnel in the Cloudflare dashboard.

### Config changes applied

In `~/Library/Application Support/plug/config.toml`:
- `tool_filter_enabled = false` — all tools visible to all clients, no search/filter
- `tool_search_threshold = 500` — raised from 50 in case filtering is re-enabled
- `meta_tool_mode = false` — no meta-tool replacement
- Figma server `enabled = false`

## Steps to Reproduce (original bugs)

1. Configure plug with 170+ tools from multiple upstream servers
2. Set PAGE_SIZE to 100 (the old default)
3. Connect Claude Desktop via Cloudflare tunnel to `plug serve`
4. Observe: tools appear initially, then UI blanks out after first tool call
5. Connect Claude Code via `plug connect` → daemon
6. Observe: tools past position 100 are not visible

## Verification

After fixes:
```bash
# Initialize HTTP session
curl -s http://127.0.0.1:3282/mcp -X POST \
  -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{...}}'

# List tools (all 184 in one page, no nextCursor)
curl -s http://127.0.0.1:3282/mcp -X POST \
  -H 'Content-Type: application/json' \
  -H 'Mcp-Session-Id: <session-id>' \
  -H 'Mcp-Protocol-Version: 2025-11-25' \
  -d '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}'
```

Expected: 184 tools, `nextCursor: null`, no UI blanking on Claude Desktop.
