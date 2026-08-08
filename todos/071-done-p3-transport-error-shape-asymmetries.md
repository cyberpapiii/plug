---
status: done
priority: p3
issue_id: "071"
tags: [http, ipc, stdio, protocol, error-handling, parity]
dependencies: []
---

# Downstream transports return different error shapes for the same failure

## Problem Statement

Two parity residuals from the 2026-07 improve program were re-verified on `main` and are
still open. Neither is client-breaking today, but both are places where a downstream
client gets a different answer depending only on which transport it used.

## Findings

Verified on `main` @ `e3b562e` on 2026-08-08.

### Finding 1 — HTTP returns a plain-text 400 for an unparsable body

`plug-core/src/http/server.rs:901-904` maps a `serde_json::from_slice` failure to
`HttpError::BadRequest("invalid JSON-RPC message")`, and `:938-941` does the same for
valid JSON of the wrong shape. `plug-core/src/http/error.rs:139` renders that as
`(StatusCode::BAD_REQUEST, msg.as_str())` and `:145` turns it into a `text/plain` body —
no `jsonrpc`/`error`/`id` envelope.

This is inconsistent **within the HTTP layer itself**: `error.rs:58-116` builds real
JSON-RPC error bodies for `Unauthorized`, `UnauthorizedWithMetadata`, and
`InsufficientScopeWithMetadata`, and `server.rs:1107-1132` (`header_mismatch_response`)
returns a 400 *with* a JSON-RPC envelope. Same layer, same status code, two body shapes.

The other transports differ again, so there is no single symmetric target to copy:

- **stdio** — owned by pinned `rmcp = "=3.1.0"` (`Cargo.toml:15`), not by plug.
  `rmcp-3.1.0/src/transport/async_rw.rs:164-189` splits on serde error category: `Syntax |
  Eof` is **silently ignored** with no response; `Data | Io` returns JSON-RPC
  `invalid_request` with `id: null`.
- **IPC** — the wire format is a typed `IpcRequest`/`IpcResponse` enum, not JSON-RPC, so
  it has no envelope to return. A frame that fails to deserialize yields
  `IpcResponse::Error { code: "PARSE_ERROR" }` and breaks the connection loop
  (`plug/src/daemon/mod.rs:733-744`); an oversized/truncated frame yields
  `MALFORMED_FRAME` (`:723-728`).

No test anywhere pins the HTTP malformed-body response shape — the `BAD_REQUEST`
assertions at `server.rs:3533, 3600, 3627, 3665, 3679, 4269, 5532` all cover
session/header/metadata rejections, not body parse failure. This was explicitly deferred
as needing an operator decision at
`docs/plans/2026-07-dispatch-unification-design-claude-fable.md:158,233`.

### Finding 2 — the IPC MCP dispatch table has no `ping` arm

`plug/src/daemon/mcp_dispatch.rs:171` enumerates the supported methods and does not
include `ping`; the catch-all at `:561-564` returns
`UNSUPPORTED_METHOD: "MCP method 'ping' not supported via IPC proxy"`.

**It is not client-reachable**, which downgrades this from a bug to a latent hole. A
downstream client talks stdio to `plug connect`, whose `IpcProxyHandler`
(`plug/src/ipc_proxy.rs:847`) does not override `ping`, so rmcp's default
(`rmcp-3.1.0/src/handler/server.rs:311-316` → `Ok(())`) answers locally and no
`IpcRequest::McpRequest` is ever sent. The dispatch arm is reachable only via the
`plug/legacy/ping` custom-request escape hatch (`ipc_proxy.rs:1191-1219`) or by a
non-plug process speaking the raw IPC protocol to the socket. (`IpcRequest::Ping` at
`plug/src/daemon/mod.rs:1296` is an unrelated IPC liveness frame, not the MCP method.)

All three transports therefore return a successful empty result for MCP `ping` as a
client sees it: stdio and IPC via rmcp's default, HTTP explicitly at
`plug-core/src/http/server.rs:1941-1957` (which deliberately returns
`method_not_found` in the modern era, since MCP 2026-07-28 removes `ping`).

The parity tests at `plug/src/daemon/mod.rs:3755-3785` cover `tools/call` only, so
nothing guards this either way.

Note: `plans/README-claude-fable.md:51` and
`docs/plans/2026-07-dispatch-unification-design-claude-fable.md:71,187` overstate this
residual by assuming the IPC dispatcher sits on the client's ping path. It does not.

### Finding 3 — modern-era HTTP differs from stdio/IPC in two places

Surfaced while verifying the protocol-era gate (which is otherwise symmetric and
correct — see the note in `docs/PROJECT-STATE-SNAPSHOT.md`):

- With the gate **on**, HTTP rejects a modern `initialize` with `method_not_found`
  (`server.rs:1895-1901`) — modern clients must use `server/discover`. stdio and IPC
  accept **both** a modern `initialize` (`plug-core/src/proxy/handler.rs:389`,
  `plug/src/ipc_proxy.rs:879`) and `discover`. Deliberate lifecycle difference, but an
  asymmetry.
- HTTP's `DiscoverResult` advertises only `["2026-07-28"]` (`server.rs:1884-1885`), while
  stdio/IPC advertise `["2025-11-25", "2026-07-28"]` (`handler.rs:376-379`,
  `ipc_proxy.rs:1005-1008`).

Also noted: `ensure_supported_downstream_protocol` (`plug-core/src/protocol.rs:562-572`)
is now dead for its stated purpose. Both call sites (`handler.rs:397`,
`ipc_proxy.rs:887`) sit inside an `else` already guarded by
`if request.protocol_version == V_2026_07_28`, so the inner comparison can never fire.
Harmless, but a cleanup candidate.

## Proposed Solutions

### Finding 1

**Option A (recommended):** route body-parse failures through the existing JSON-RPC error
envelope, following the precedent already set by `header_mismatch_response`
(`server.rs:1107-1132`), using JSON-RPC `-32700 Parse error` for syntax failures and
`-32600 Invalid Request` for shape failures, with `id: null`. Pin the shape with a test.
Externally visible for remote HTTP clients — strictly more standard, but it is a contract
change and wants a release note.

**Option B:** leave the behavior and add a test that pins the current plain-text 400, so
it stops being unspecified.

Cross-transport symmetry is **not** achievable here: stdio's behavior lives in pinned rmcp
code and IPC is not JSON-RPC at all. The achievable goal is internal HTTP consistency.

### Finding 2

Add a `ping` arm to the IPC dispatch table returning an empty result, plus a parity test.
Small and safe; closes the hole without changing any client-visible behavior.

### Finding 3

Decide whether the modern `initialize` acceptance and the advertised-version list should
converge, or be documented as intentional per-transport lifecycle differences. Remove or
comment `ensure_supported_downstream_protocol`.

## Acceptance Criteria

- [x] HTTP malformed-body response shape is pinned by a test, whichever option is chosen
- [x] IPC dispatch handles `ping`, with a parity test alongside the existing `tools/call` ones
- [x] Modern-era `initialize` handling and advertised-version lists are either converged or
      documented as intentional
- [x] `plans/README-claude-fable.md:51` and the dispatch-unification design doc are corrected
      to say the IPC `ping` gap is not client-reachable

## Resources

- `plug-core/src/http/server.rs`, `plug-core/src/http/error.rs`
- `plug/src/daemon/mcp_dispatch.rs`, `plug/src/ipc_proxy.rs`
- `docs/plans/2026-07-dispatch-unification-design-claude-fable.md`

## Work Log

### 2026-08-08 - Tracked

**By:** Claude Fable 5

Re-verified two improve-program residuals and gave them a tracked home; added finding 3
from the protocol-era verification pass. No code change made — finding 1 needs an operator
decision on whether to change an externally visible response shape.

### 2026-08-08 - Resolved

**By:** Claude Fable 5

All three findings closed on `main`.

**Finding 1 — Option A.** Added `HttpError::MalformedJsonRpc { code, message }` to
`plug-core/src/http/error.rs`, which renders a JSON-RPC error envelope with `id: null`
instead of a plain-text body. Both parse sites in `plug-core/src/http/server.rs` now use
it: `-32700 Parse error` for malformed JSON, `-32600 Invalid Request` for well-formed JSON
of the wrong shape. `HttpError::BadRequest` is untouched, so every other 400 keeps its
existing plain-text body. Pinned by `malformed_body_returns_jsonrpc_error_envelope`.

This is an externally visible contract change for remote HTTP clients and wants a release
note: a body that used to come back as `text/plain` now comes back as
`application/json`. The status code is unchanged at 400.

**Finding 2.** Added a `ping` arm to `plug/src/daemon/mcp_dispatch.rs` returning an empty
result in the legacy era and `UNSUPPORTED_METHOD` in the modern era, matching both the HTTP
adapter and the MCP 2026-07-28 removal of the method. Added a `ping` arm to the stdio
parity driver (it had none and panicked) and the parity test
`parity_ping_matches_across_transports`. No client-visible behavior changed, since the arm
was only reachable through `plug/legacy/` or a raw IPC speaker.

**Finding 3.** The advertised-version lists are now converged: all three transports derive
theirs from the new `plug_core::protocol::supported_downstream_protocol_versions`, so
HTTP's `server/discover` reports `["2025-11-25", "2026-07-28"]` like stdio and IPC. Legacy
belongs in that list — the same port still serves it through `initialize`, and omitting it
told a modern client that support had been dropped.

The modern-`initialize` difference is documented as intentional rather than converged, with
comments at both stdio and IPC `initialize` sites. HTTP classifies an era from the
`MCP-Protocol-Version` header and can honestly answer `method_not_found` for a method the
modern revision deleted. Stdio and IPC have no era to classify — RMCP's stdio service only
knows the initialize lifecycle — so rejecting a `2026-07-28` initialize there would leave a
modern-aware local client with no way to connect at all.

`ensure_supported_downstream_protocol` and its two tests were deleted from
`plug-core/src/protocol.rs`, and the now-redundant `else` nesting was flattened at both
call sites.

The overstated `ping` claims in `plans/README-claude-fable.md` and
`docs/plans/2026-07-dispatch-unification-design-claude-fable.md` were corrected in place
with dated notes rather than rewritten, since both are historical records.
