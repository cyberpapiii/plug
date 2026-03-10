# Bug: Claude remote MCP connector shows "no tools available" when plug initialize body advertises `2025-06-18`

**Repository:** `plug-mcp/plug`  
**Affected surface:** downstream Streamable HTTP server (`plug serve`) behind a public remote connector  
**Severity:** High — connector appears to connect but Claude exposes no tools  
**Observed in:** Claude Desktop/Web remote custom connector, March 10, 2026

## Summary

`plug`'s downstream HTTP `/mcp` endpoint successfully accepts `initialize` and `tools/list`, but the
JSON body of the `initialize` response advertises `protocolVersion: "2025-06-18"` even though the
HTTP layer otherwise implements the `2025-11-25` downstream contract and sends the
`MCP-Protocol-Version: 2025-11-25` header.

In practice, Claude's remote MCP connector connects to the endpoint but then reports:

- "This connector has no tools available"

The endpoint itself does return real tools when probed manually, so the most plausible root cause
is that Claude rejects or abandons the connector after seeing the stale protocol version in the
`initialize` response body.

## Root Cause

`plug`'s downstream HTTP handler builds the initialize response body using rmcp's
`InitializeResult::new(...)`, which defaults `protocol_version` to rmcp's current `LATEST`
constant (`2025-06-18` in rmcp 1.1.x).

`plug` does define and enforce `2025-11-25` at the HTTP transport layer, but that value was only
applied to the `MCP-Protocol-Version` response header — not to the JSON body field
`result.protocolVersion`.

So the HTTP response was internally inconsistent:

- header: `MCP-Protocol-Version: 2025-11-25`
- JSON body: `"protocolVersion": "2025-06-18"`

## Evidence

### Manual public endpoint probe

Before the local fix, probing the public `/mcp` endpoint returned:

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "protocolVersion": "2025-06-18",
    "capabilities": {
      "logging": {},
      "completions": {},
      "prompts": { "listChanged": true },
      "resources": { "subscribe": true, "listChanged": true },
      "tools": { "listChanged": true }
    },
    "serverInfo": { "name": "plug", "version": "0.1.0" }
  }
}
```

But the same endpoint successfully answered `tools/list` with a large real tool list when called
manually using the returned `Mcp-Session-Id`.

That means the connector failure was not "no tools exist"; it was "Claude did not surface them."

### Code path in plug

**Downstream HTTP initialize path**

- [`plug-core/src/http/server.rs:763`](../../plug-core/src/http/server.rs#L763) builds the initialize result
- [`plug-core/src/http/server.rs:994`](../../plug-core/src/http/server.rs#L994) calls `InitializeResult::new(...)`
- [`plug-core/src/http/server.rs:37`](../../plug-core/src/http/server.rs#L37) defines the HTTP-layer protocol constant as `2025-11-25`
- [`plug-core/src/http/server.rs:1071`](../../plug-core/src/http/server.rs#L1071) only injected that value into the response header

### rmcp default

rmcp 1.1.x still defaults `InitializeResult.protocol_version` to `LATEST = 2025-06-18`:

- `rmcp-1.1.1/src/model.rs:790`
- `rmcp-1.1.1/src/model.rs:796`
- `rmcp-1.1.1/src/model.rs:153-156`

## Reproduction

1. Start `plug serve` on a publicly reachable URL
2. Add that URL as a Claude remote MCP custom connector
3. Claude shows the connector entry but reports no tools available
4. Manually probe:

```bash
curl -s -X POST https://<public-url>/mcp \
  -H 'content-type: application/json' \
  --data '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"probe","version":"1.0"}}}'
```

5. Observe `protocolVersion: "2025-06-18"` in the body
6. Continue with `tools/list` using the returned session ID and observe that real tools are present

## Expected Behavior

The downstream HTTP initialize response should advertise a protocol version consistent with the
transport surface that `plug` actually implements for remote clients:

- `protocolVersion: "2025-11-25"`

both in the HTTP header and in the JSON body.

## Fix

Patch the downstream HTTP initialize response construction so the serialized JSON body explicitly
overrides:

```json
"result": {
  "protocolVersion": "2025-11-25"
}
```

The local patch for this was applied in:

- [`plug-core/src/http/server.rs`](../../plug-core/src/http/server.rs)

with an added regression assertion in the HTTP tests verifying:

- `initialize_response_contains_server_info` now also checks for `"2025-11-25"`

## Notes

- This bug affects the downstream HTTP server path used by Claude remote connectors.
- It is separate from upstream rmcp client negotiation or the local daemon-backed stdio paths.
- Quick tunnels and local auth issues were investigated and ruled out once the endpoint was shown
  to return real tools to manual `tools/list` calls.
