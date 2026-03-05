# Bug: Tool calls block for 30-60+ seconds when OAuth token expires due to synchronous re-authentication

**Repository:** modelcontextprotocol/mcp-remote
**Version:** 0.1.38
**Severity:** High — makes mcp-remote unusable as a headless/daemon-mode stdio proxy
**Affects:** Any MCP server behind OAuth (Supabase, Figma, Notion, Krisp, Supermemory, etc.)

## Summary

When an OAuth access token expires during a tool call, mcp-remote synchronously blocks the entire stdio transport to run a full browser-based OAuth re-authentication flow. This causes tool calls to hang for 30-60+ seconds (or fail entirely if no browser is available). In headless/daemon environments where mcp-remote is spawned as a child process, the browser OAuth prompt never completes, causing the tool call to time out.

This is the single biggest reliability issue for anyone running mcp-remote in a multiplexer, daemon, or headless context — which is how most multi-client AI workflows use it.

## Environment

- mcp-remote: 0.1.38 (via `npx -y mcp-remote`)
- Upstream servers tested: Supabase (`https://mcp.supabase.com/mcp`), Figma (`https://figma.com/...`)
- macOS 15.4, Node.js v22
- Running as stdio child process of an MCP multiplexer (no browser access)

## Root Cause

### The blocking re-auth flow

When `StreamableHTTPClientTransport.send()` (POST) receives a 401, it synchronously calls the `auth()` function which starts an OAuth callback server and waits for the user to complete browser authentication:

**File:** `dist/chunk-65X3S4HB.js`, lines 19852-19869

```javascript
// Inside StreamableHTTPClientTransport.send()
if (response.status === 401 && this._authProvider) {
  if (this._hasCompletedAuthFlow) {
    throw new StreamableHTTPError(401, "Server returned 401 after successful authentication");
  }
  const { resourceMetadataUrl, scope } = extractWWWAuthenticateParams(response);
  this._resourceMetadataUrl = resourceMetadataUrl;
  this._scope = scope;
  const result = await auth(this._authProvider, {      // <-- BLOCKS here
    serverUrl: this._url,
    resourceMetadataUrl: this._resourceMetadataUrl,
    scope: this._scope,
    fetchFn: this._fetchWithInit
  });
  if (result !== "AUTHORIZED") {
    throw new UnauthorizedError();
  }
  this._hasCompletedAuthFlow = true;
  return this.send(message);  // recursive retry after re-auth
}
```

The `auth()` call starts an HTTP callback server on localhost, opens a browser, and waits for the OAuth redirect — with a default timeout of 30 seconds:

**File:** `dist/chunk-65X3S4HB.js`, lines 20618-20621

```javascript
const longPollTimeout = setTimeout(() => {
  log("Long poll timeout reached, responding with 202");
  res.status(202).send("Authentication in progress");
}, options.authTimeoutMs || 3e4);  // 30 seconds default
```

### Why this is especially problematic

1. **No background token refresh:** When the access token expires, mcp-remote doesn't attempt to use the refresh token first. It goes straight to the full browser OAuth flow. The `tokens()` method reads from disk but doesn't check expiry or attempt refresh:

   ```javascript
   // lines 21068-21093
   async tokens() {
     const tokens = await readJsonFile(this.serverUrlHash, "tokens.json", OAuthTokensSchema);
     // reads tokens, logs debug info, but DOES NOT attempt refresh if expired
     return tokens;
   }
   ```

2. **Headless environments can't complete the flow:** When mcp-remote is spawned as a stdio child process (e.g., by a multiplexer daemon), there's no browser context. The OAuth callback server starts, the browser open fails silently, and the 30-second timeout expires. Meanwhile, the tool call is blocked.

3. **Inconsistent 401 handling between POST and GET:** The POST handler (lines 19852-19869) sets `_hasCompletedAuthFlow = true` after one successful re-auth and throws on subsequent 401s. But the GET/SSE handler (lines 19676-19678) has no such guard:

   ```javascript
   // GET handler - no guard against repeated re-auth
   if (response.status === 401 && this._authProvider) {
     return await this._authThenStart();  // can retry indefinitely
   }
   ```

4. **Token expiry is stored as a countdown, not an absolute timestamp:** The `expires_in` field is a relative number of seconds from when the token was issued, but it's stored as-is in `tokens.json` without recording the issue time. This means mcp-remote can't reliably determine whether a cached token is still valid without making a request and getting a 401.

### Token storage structure

Tokens are stored in `~/.mcp-auth/{version}/{serverUrlHash}_tokens.json`:

```javascript
// serverUrlHash is computed from:
function getServerUrlHash(serverUrl, authorizeResource, headers) {
  const parts = [serverUrl];
  if (authorizeResource) parts.push(authorizeResource);
  if (headers && Object.keys(headers).length > 0) {
    const sortedKeys = Object.keys(headers).sort();
    parts.push(JSON.stringify(headers, sortedKeys));
  }
  return crypto2.createHash("md5").update(parts.join("|")).digest("hex");
}
```

Currently stored on this machine:
```
~/.mcp-auth/mcp-remote-0.1.37/
  1c6f0dcbe13bc5ddd0e9514242affa9f_tokens.json
  7a135ace865ed8bc3f3b89491c553d66_tokens.json
  cb42d1a06ae8db4e5585a26f2e5ca947_tokens.json
  e8fe1423b57023b6315cd9029dbc1859_tokens.json
```

## Impact: Supabase MCP Server

When calling `supabase__list_organizations` through mcp-remote:

| Step | Time | What happens |
|------|------|-------------|
| Tool call sent via stdio | 0s | MCP multiplexer sends `tools/call` |
| mcp-remote POSTs to `https://mcp.supabase.com/mcp` | 0.5s | StreamableHTTP transport |
| Supabase returns 401 (token expired) | 2s | Token was valid last session |
| mcp-remote starts OAuth re-auth | 2s | `auth()` called synchronously |
| OAuth discovery + callback server started | 5s | `discoverOAuthProtectedResourceMetadata()` |
| Browser open attempted (fails in headless) | 5.5s | No browser available |
| Waiting for OAuth callback... | 5-35s | 30-second timeout ticking |
| Timeout expires | 35s | Returns 202 "Authentication in progress" |
| Recursive retry (may fail again) | 35-65s | Or throws UnauthorizedError |
| **Total** | **35-65+ seconds** | |

## Impact: Figma MCP Server

Same flow, but Figma additionally returns HTTP 403 (Forbidden) when the initial auth has never been completed in a browser session. The `connectToRemoteServer` function catches this and tries auth recursion (lines 20548-20598):

```javascript
if (recursionReasons.has(REASON_AUTH_NEEDED)) {
  const errorMessage = `Already attempted reconnection for reason: ${REASON_AUTH_NEEDED}. Giving up.`;
  log(errorMessage);
  throw new Error(errorMessage);
}
recursionReasons.add(REASON_AUTH_NEEDED);
return connectToRemoteServer(client, serverUrl, authProvider, headers,
  authInitializer, transportStrategy, recursionReasons);
```

This means Figma requires one successful interactive browser OAuth session before mcp-remote can work headlessly at all. But after that first session, the same token-expiry-blocks-tool-call issue applies.

## Steps to Reproduce

```bash
# 1. Start mcp-remote as stdio process (simulating headless/daemon mode)
echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"1.0"}}}' | npx -y mcp-remote https://mcp.supabase.com/mcp

# 2. Wait for token to expire (or delete cached tokens)
rm -rf ~/.mcp-auth/mcp-remote-*/

# 3. Send a tool call — observe 30-60+ second hang
echo '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"list_organizations","arguments":{}}}' | npx -y mcp-remote https://mcp.supabase.com/mcp
```

## Expected Behavior

1. When access token expires, mcp-remote should **attempt refresh token flow first** (no browser needed) before falling back to browser OAuth
2. If refresh fails and browser is unavailable, return an error immediately rather than blocking for 30 seconds
3. Store token expiry as an absolute timestamp so cached tokens can be validated without a round-trip
4. In headless/daemon mode, detect that browser is unavailable and skip the browser-open step

## Proposed Solutions

### 1. Implement refresh token flow (most impactful)

Before triggering full browser OAuth on 401, attempt a token refresh using the stored refresh token:

```javascript
async tokens() {
  const tokens = await readJsonFile(this.serverUrlHash, "tokens.json", OAuthTokensSchema);
  if (tokens && tokens.refresh_token && this.isExpired(tokens)) {
    try {
      const refreshed = await this.refreshAccessToken(tokens.refresh_token);
      await this.saveTokens(refreshed);
      return refreshed;
    } catch (e) {
      // Refresh failed, fall through to browser auth
    }
  }
  return tokens;
}
```

### 2. Add headless mode flag

```bash
npx -y mcp-remote https://mcp.supabase.com/mcp --headless
```

In headless mode: attempt refresh token only. If that fails, return an MCP error immediately with a message like "OAuth re-authentication required — run `npx mcp-remote <url>` interactively to refresh credentials."

### 3. Store absolute token expiry

```javascript
async saveTokens(tokens) {
  tokens._expires_at = Date.now() + (tokens.expires_in * 1000);
  await writeJsonFile(this.serverUrlHash, "tokens.json", tokens);
}
```

### 4. Consistent 401 handling between POST and GET

Apply the same `_hasCompletedAuthFlow` guard to the GET/SSE handler to prevent infinite re-auth loops.
