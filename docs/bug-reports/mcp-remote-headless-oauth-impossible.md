# Bug: mcp-remote cannot complete initial OAuth in headless/daemon environments

**Repository:** modelcontextprotocol/mcp-remote
**Version:** 0.1.38
**Severity:** Medium — blocks first-time setup in any non-interactive context
**Affects:** Figma, Supabase, and any OAuth-protected MCP server when run headlessly
**Related:** See companion bug report on token-expiry re-auth blocking

## Summary

mcp-remote requires an interactive browser session to complete the initial OAuth authorization for each MCP server. When spawned as a stdio child process in a daemon, multiplexer, or CI environment, the browser-based OAuth flow cannot complete. There is no way to pre-authenticate, import tokens, or skip the browser step.

This makes it impossible to use mcp-remote with OAuth-protected servers in any environment that doesn't have an interactive desktop session — which includes MCP multiplexers, daemon processes, Docker containers, SSH sessions, and CI/CD pipelines.

## Root Cause

### The OAuth flow requires browser interaction

**File:** `dist/chunk-65X3S4HB.js`, `connectToRemoteServer()` lines 20548-20598

When mcp-remote encounters an `UnauthorizedError` during initial connection, it calls `authInitializer()` which:

1. Starts an Express HTTP server on localhost for the OAuth callback
2. Opens the system browser to the OAuth provider's authorization URL
3. Waits (up to 30 seconds by default) for the OAuth redirect to hit the callback server
4. Exchanges the authorization code for tokens
5. Stores tokens in `~/.mcp-auth/{version}/{hash}_tokens.json`

```javascript
// connectToRemoteServer - auth recursion (lines 20569-20588)
if (recursionReasons.has(REASON_AUTH_NEEDED)) {
  const errorMessage = `Already attempted reconnection for reason: ${REASON_AUTH_NEEDED}. Giving up.`;
  log(errorMessage);
  throw new Error(errorMessage);  // <-- Dies here in headless mode
}
recursionReasons.add(REASON_AUTH_NEEDED);
return connectToRemoteServer(client, serverUrl, authProvider, headers,
  authInitializer, transportStrategy, recursionReasons);
```

### No alternative auth methods exist

There is no way to:
- Import an existing OAuth token from another tool
- Provide a pre-obtained access/refresh token via environment variable or CLI flag
- Use a service account or API key instead of OAuth
- Complete the OAuth flow in a separate process and have mcp-remote pick up the tokens

### The callback server conflicts with headless mode

The OAuth callback server binds to `http://localhost:{port}/oauth/callback`:

```javascript
// lines 20958-20960
get redirectUrl() {
  return `http://${this.options.host}:${this.options.callbackPort}${this.callbackPath}`;
}
```

In a multiplexer/daemon context, this localhost server works fine — the port is available. But the browser that would redirect to it doesn't exist, so the callback never fires.

## Impact

### Figma MCP Server

Figma is the clearest example. The server returns 403 on first connection without valid OAuth tokens. mcp-remote attempts browser auth, fails in headless mode, retries once (recursion guard), then throws:

```
Error: Already attempted reconnection for reason: REASON_AUTH_NEEDED. Giving up.
```

The user must:
1. Stop the daemon/multiplexer
2. Run `npx mcp-remote https://figma.com/...` manually in a terminal
3. Complete browser OAuth
4. Restart the daemon/multiplexer to pick up the cached tokens

This breaks the "just configure and go" promise of MCP.

### All OAuth-protected servers (on first use or token rotation)

Any server using mcp-remote with OAuth (Supabase, Notion, Krisp, Supermemory) has the same issue on first use. After initial interactive auth, tokens are cached and work headlessly — until they expire and the re-auth problem from the companion bug report kicks in.

## Steps to Reproduce

```bash
# 1. Clear any cached tokens
rm -rf ~/.mcp-auth/

# 2. Run mcp-remote as a stdio child process (simulating daemon mode)
echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"1.0"}}}' | npx -y mcp-remote https://figma.com/mcp

# 3. Observe: connection attempt, browser open fails silently, 30-second hang, then error
```

## Expected Behavior

mcp-remote should support at least one non-interactive authentication method for daemon/headless environments.

## Proposed Solutions (any one of these would solve the problem)

### 1. Token import command

```bash
# Authenticate interactively once, export tokens
npx mcp-remote auth https://figma.com/mcp --export > figma-tokens.json

# Import tokens in headless mode
npx mcp-remote https://figma.com/mcp --import-tokens figma-tokens.json
```

### 2. Environment variable for pre-obtained tokens

```bash
MCP_REMOTE_ACCESS_TOKEN="ya29.a0..." npx mcp-remote https://figma.com/mcp
MCP_REMOTE_REFRESH_TOKEN="1//0..." npx mcp-remote https://figma.com/mcp
```

### 3. Separate `auth` subcommand

```bash
# Run this interactively once (in a terminal with browser access)
npx mcp-remote auth https://figma.com/mcp

# Then run headlessly — tokens are already cached
npx mcp-remote https://figma.com/mcp --headless
```

This is the pattern used by `gcloud auth login`, `aws sso login`, `gh auth login`, etc. — separate the auth step from the runtime step.

### 4. Detect headless mode and fail fast

At minimum, detect that no browser is available and return an immediate, actionable error instead of hanging for 30 seconds:

```
Error: OAuth authentication required but no browser available.
Run 'npx mcp-remote auth https://figma.com/mcp' interactively first,
then restart in headless mode.
```

## Workaround (Current)

The only workaround today is:

1. Run `npx mcp-remote <server-url>` interactively in a terminal
2. Complete the browser OAuth flow
3. Verify tokens are cached in `~/.mcp-auth/`
4. Then start the daemon/multiplexer — it will use the cached tokens

This breaks on token expiry (see companion bug report) and requires manual intervention every time tokens rotate.
