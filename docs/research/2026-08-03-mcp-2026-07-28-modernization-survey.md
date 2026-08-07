# MCP 2026-07-28 — Modernization Survey for Plug

**Date**: 2026-08-03
**Status**: Research. No code changed. Nothing here is "done on main".
**Trigger**: [`@ClaudeDevs` announcement](https://x.com/ClaudeDevs/status/2082164248697069935) (2026-07-28), [MCP blog](https://blog.modelcontextprotocol.io/posts/2026-07-28/), [spec changelog](https://modelcontextprotocol.io/specification/2026-07-28/changelog).

---

## 1. Executive Summary

MCP `2026-07-28` is the largest revision since launch. It is **not** an additive
release — it deletes the `initialize` handshake, protocol sessions, the GET SSE
endpoint, `ping`, `logging/setLevel`, SSE resumability, and server-initiated
requests. The protocol is now stateless request/response with per-request `_meta`.

For Plug this is unusually consequential, because Plug is a **proxy**: it is a
downstream *server* to Claude Desktop/Code and an upstream *client* to ~20 MCP
servers. Every wire-level change lands twice, in opposite directions.

Three things matter most:

1. **`rmcp` 3.1.0 is out and stable** (2026-07-31) and implements `2026-07-28`.
   Plug pins `=2.2.0`. This upgrade gates essentially all of the rest.
2. **Plug's real opportunity is dual-era bridging.** The spec's own compatibility
   matrix says Modern-client→Legacy-server and Legacy-client→Modern-server both
   **fail**. Plug can make both work. That is a genuine product differentiator,
   not just a migration chore.
3. **A handful of changes are pure wins available today**, independent of the
   version bump — cacheable list results, deterministic tool ordering, OAuth
   `iss` validation, and DCR `application_type`.

Current posture on `main` is deliberate and correct: `plug-core/src/protocol.rs:14`
actively **rejects** downstream clients requesting `2026-07-28`, and
`docs/MCP-SPEC.md:20` documents why. That guard was written when the revision was
merely announced. It is now stable and shipping in Claude products, so the guard
has moved from "prudent" to "a countdown clock".

---

## 2. What Actually Changed in 2026-07-28

### 2.1 Breaking — the stateless core

| Change | Detail |
| --- | --- |
| `initialize` / `notifications/initialized` removed | Every request carries `io.modelcontextprotocol/protocolVersion`, `clientInfo`, `clientCapabilities` in `_meta` |
| `Mcp-Session-Id` removed | No protocol-level sessions; HTTP DELETE gone. Cross-call state uses server-minted handles passed as ordinary tool arguments |
| `server/discover` added | Servers **MUST** implement. Returns `supportedVersions`, `capabilities`, `serverInfo` in `_meta`, `instructions`. Optional for clients; used as the stdio backward-compat probe |
| GET SSE endpoint removed | Replaced by `subscriptions/listen` — a long-lived POST-response stream the client opts into per notification type (`toolsListChanged`, `promptsListChanged`, `resourcesListChanged`, `resourceSubscriptions`) |
| `resources/subscribe` / `unsubscribe` removed | Folded into `subscriptions/listen` |
| `ping`, `logging/setLevel`, `notifications/roots/list_changed` removed | Log level now per-request via `io.modelcontextprotocol/logLevel` in `_meta`. Servers **MUST NOT** emit `notifications/message` for requests lacking it |
| SSE resumability removed | No `Last-Event-ID`, no event IDs. A broken stream loses the request; client re-issues with a **new** request ID |
| Server-initiated requests removed | Replaced by MRTR (below). This is called out as a breaking change in the spec |
| `resultType` required on all results | `"complete"` or `"input_required"`. Results from earlier-protocol servers omitting it **MUST** be treated as `"complete"` |
| Tasks moved out of core | Now the `io.modelcontextprotocol/tasks` extension |

### 2.2 Multi Round-Trip Requests (MRTR)

Replaces `elicitation/create`, `sampling/createMessage`, `roots/list` as
server-initiated requests. Instead:

1. Server returns `InputRequiredResult` (`resultType: "input_required"`) with an
   `inputRequests` map (server-assigned keys → request objects) and an opaque
   `requestState` string.
2. Client gathers the inputs, then **retries the original request** with
   `inputResponses` and the echoed `requestState`, using a **different JSON-RPC id**.
3. Server reconstitutes state from `requestState` and completes.

Only valid on `tools/call`, `resources/read`, `prompts/get`. `requestState` must
be treated as attacker-controlled — integrity-protected (HMAC/AEAD), bound to the
authenticated principal, with a short TTL and an originating-request identifier.

### 2.3 HTTP transport metadata

Required on every Streamable HTTP POST:

- `MCP-Protocol-Version` — **must match** `_meta`'s protocolVersion or reject with `-32020`
- `Mcp-Method` — mirrors `method`
- `Mcp-Name` — mirrors `params.name` / `params.uri` (for `tools/call`, `resources/read`, `prompts/get`)

Plus `x-mcp-header`: servers may annotate tool `inputSchema` properties to be
mirrored into `Mcp-Param-{Name}` headers. **Clients MUST support this** — including
rejecting (excluding from `tools/list`) any tool whose annotations violate the
constraints. Non-ASCII values use the `=?base64?…?=` sentinel encoding.

Servers that only speak this revision should answer GET/DELETE on the MCP endpoint
with `405`, ignore `Mcp-Session-Id`, and ignore `Last-Event-ID`.

### 2.4 Caching

`tools/list`, `prompts/list`, `resources/list`, `resources/read`,
`resources/templates/list` now **require** `ttlMs` and `cacheScope`
(`"public"` | `"private"`) via a `CacheableResult` interface. `server/discover`
is also cacheable. Servers **SHOULD** return tools in deterministic order to
improve LLM prompt-cache hit rates.

### 2.5 Authorization hardening

| Change | Requirement |
| --- | --- |
| RFC 9207 `iss` | AS **SHOULD** return `iss`; clients **MUST** validate it against the recorded issuer before redeeming the code |
| DCR `application_type` | Clients **MUST** specify it — this is what fixes `localhost` redirect_uri rejection for desktop/CLI apps |
| Issuer binding | Credentials **MUST** be keyed by issuer, **MUST NOT** be reused across ASes, **MUST** re-register when the AS changes |
| DCR deprecated | In favor of Client ID Metadata Documents (CIMD). Still available for backwards compat |

### 2.6 Error codes

Policy: `-32000..-32019` implementation-defined (grandfathered), `-32020..-32099`
reserved for the spec.

- Resource not found: `-32002` → **`-32602`** (Invalid Params)
- `HeaderMismatch`: **`-32020`**
- `MissingRequiredClientCapability`: **`-32021`**
- `UnsupportedProtocolVersion`: **`-32022`** (with `data.supported` / `data.requested`)

### 2.7 Deprecations (≥12-month window, formal lifecycle policy)

- **Roots, Sampling, Logging** — functional but new implementations should not adopt
- **HTTP+SSE transport** (2024-11-05) — reclassified Deprecated
- **`includeContext`** values `"thisServer"` / `"allServers"`
- **OAuth DCR** — in favor of CIMD

### 2.8 Extensions framework

`extensions` field added to `ClientCapabilities` and `ServerCapabilities` — a map
of prefixed identifiers → settings objects. Official extensions:
`io.modelcontextprotocol/tasks`, `io.modelcontextprotocol/ui` (MCP Apps),
Enterprise Managed Auth.

Tasks extension redesign: `tasks/result` (blocking) → polling via `tasks/get`;
new `tasks/update` for client→server input; **`tasks/list` removed**; servers may
return task handles unsolicited without per-request opt-in.

### 2.9 Misc

- OpenTelemetry trace context conventions for `_meta` (`traceparent`, `tracestate`, `baggage`)
- `inputSchema`/`outputSchema` loosened to any JSON Schema 2020-12 keywords; `structuredContent` any JSON value; `$ref` resolution requirements added
- `notifications/elicitation/complete` and `elicitationId` removed (superseded by MRTR retry)

---

## 3. Where Plug Stands (verified on `main`, 2026-08-03)

| Area | Status | Evidence |
| --- | --- | --- |
| Negotiated wire revision | `2025-11-25` | `plug-core/src/protocol.rs:6` |
| `2026-07-28` downstream requests | **Actively rejected** | `plug-core/src/protocol.rs:13-23` |
| `rmcp` pin | `=2.2.0` (latest is **3.1.0**) | `Cargo.toml:15` |
| MSRV | `1.88.0` — already meets rmcp 3.x | `Cargo.toml:8` |
| `Mcp-Session-Id` | Implemented downstream | `plug-core/src/http/server.rs:37` |
| GET `/mcp` SSE | Implemented | `plug-core/src/http/server.rs:413,936` |
| DELETE `/mcp` | Implemented | `plug-core/src/http/server.rs:413` |
| `Last-Event-ID` resumability | Implemented (upstream SSE client) | `plug-core/src/transport/sse_client.rs:303-435` |
| Elicitation / sampling / roots forwarding | Implemented as server-initiated, all transports | ~260 refs across `plug-core/src`, `plug/src` |
| `resources/subscribe` lifecycle | Implemented | `plug-core/src/http/server.rs:997` |
| Tasks | Implemented against the **2025-11-25 experimental core** shape (`tasks/get`, `tasks/result`, `tasks/list`, `tasks/cancel`) | `plug-core/src/proxy/tasks.rs`, `plug/src/ipc_proxy.rs:969` |
| Upstream OAuth `iss` validation | **Missing** — no RFC 9207 handling found | `plug-core/src/oauth.rs` |
| DCR `application_type` | **Missing** | `plug-core/src/oauth.rs` |
| Issuer-keyed credentials | Present downstream (`downstream_oauth` is issuer-scoped); unverified upstream | `plug-core/src/downstream_oauth/mod.rs:3` |
| CIMD | Named in `docs/MCP-SPEC.md:18` as a 2025-11-25 feature; implementation status unverified | — |
| `ttlMs` / `cacheScope` | **Missing** | — |
| Deterministic tool ordering | **Unverified** | — |
| `Mcp-Method` / `Mcp-Name` / `x-mcp-header` | **Missing** | — |
| `server/discover` | **Missing** | — |
| `subscriptions/listen` | **Missing** | — |
| MRTR | **Missing** | — |
| `extensions` capability negotiation | **Missing** | — |

Docs are honest and current: `docs/MCP-SPEC.md:20` and
`docs/PROJECT-STATE-SNAPSHOT.md:126-128` both correctly state that `2026-07-28` is
announced-but-unimplemented and that the SDK upgrade did not opt Plug in.

---

## 4. The Central Design Question: Plug as a Dual-Era Bridge

The spec's compatibility matrix:

| Client | Server | Outcome |
| --- | --- | --- |
| Modern | Legacy | **Fails** |
| Legacy | Modern | **Fails** |
| Dual-era | either | Works |

Plug sits exactly in the middle of both failing cases. If Plug becomes dual-era on
**both** faces, it makes a modern Claude Desktop talk to a legacy stdio server, and
a legacy client talk to a modern serverless upstream. Nobody else in a user's stack
is positioned to do that.

Two bridging directions, with very different difficulty:

**Modern upstream → legacy downstream client** (easy).
Upstream returns `InputRequiredResult`. Plug issues a normal server-initiated
`elicitation/create` to the legacy downstream client, collects the answer, retries
upstream with `inputResponses` + echoed `requestState`. Plug already owns all the
machinery for the downstream half.

**Legacy upstream → modern downstream client** (the hard one).
Upstream sends a server-initiated `elicitation/create` and blocks. The modern
downstream client expects `InputRequiredResult` and will terminate the request.
Plug must:
- hold the in-flight upstream request open,
- mint its own integrity-protected `requestState` (HMAC, principal-bound, TTL,
  request-digest — per §2.2 security requirements),
- return `input_required` downstream,
- correlate the client's retry (which arrives with a **different JSON-RPC id**)
  back to the parked upstream request.

Plug can do this precisely because it is a long-lived daemon with real state. The
statelessness the spec demands is a property of the *wire*, not of Plug's process.
This is a place where Plug's architecture is an asset — but the `requestState`
crypto and the parked-request correlation table are genuinely new subsystems, and
the replay/TTL/principal-binding rules are security-critical.

**Note the asymmetry**: statelessness simplifies Plug's *downstream HTTP server*
(sessions, GET streams, resumability all delete cleanly) while MRTR *complicates*
Plug's proxy core. Net line count may not go down.

---

## 5. Modernization Opportunities, Ranked

### Tier 1 — Available now, no version bump required

These are independent of `2026-07-28` negotiation and pay off immediately.

1. **OAuth `iss` validation (RFC 9207).** A genuine security gap today —
   authorization-server mix-up is exactly the attack a multi-upstream multiplexer
   is exposed to. Plug holds credentials for ~20 servers across different ASes.
   *Highest security-per-line-of-code in this document.*
2. **DCR `application_type`.** Plug uses `http://localhost:43189/callback`
   (`plug-core/src/oauth.rs:1607`). This is the exact case the spec change fixes.
   Likely resolves real registration failures against OIDC-strict providers —
   plausibly including the disabled Figma upstream noted in project memory.
3. **Upstream credential issuer binding.** Verify credentials are keyed by issuer
   and re-registered on AS change. `downstream_oauth` already claims issuer-scoping;
   the upstream side needs an audit.
4. **Deterministic tool ordering.** Plug merges hundreds of tools from ~20 upstreams.
   Stable ordering directly improves LLM prompt-cache hit rates. Cheap, and Plug is
   the ideal place to enforce it.
5. **Resource-not-found error code** `-32002` → `-32602`.

### Tier 2 — High leverage, needs the rmcp 3.x upgrade

6. **`rmcp` 2.2.0 → 3.1.0.** Gates everything below. Breaking changes to absorb:
   lifecycle redesign (no sessions), `ServerResult` matches must handle
   `InputRequiredResult`, `_meta` split into `MetaObject` / `RequestMetaObject` /
   `NotificationMetaObject`, OAuth consolidated around `AuthorizationRequest`,
   deprecated v3 APIs removed. MSRV 1.88 — **already satisfied**.
   Precedent exists: `docs/superpowers/plans/2026-07-13-rmcp-2.2-upgrade.md`.
7. **`ttlMs` / `cacheScope` on list results.** This is the standout product win.
   Project memory records that Claude Desktop's remote connector *ignores
   pagination and never subscribes to `list_changed`* — worked around today by
   forcing `PAGE_SIZE=500`. `ttlMs` is a first-class protocol answer to exactly
   that class of client. Plug can also *consume* upstream `ttlMs` to stop
   re-listing ~20 servers unnecessarily.
8. **`server/discover`.** Required of servers. For Plug it doubles as the upstream
   era-probe and a natural place to publish merged capabilities. Also cacheable.
9. **Header metadata (`Mcp-Method`, `Mcp-Name`, `x-mcp-header`).** Plug is *literally*
   the intermediary these headers were designed for — it can route, meter, and log
   on them without parsing bodies. Note `x-mcp-header` client support is **MUST**,
   including rejecting malformed tool definitions from `tools/list`.
10. **`subscriptions/listen`.** Replaces Plug's GET `/mcp` SSE and the
    `resources/subscribe` lifecycle. Plug must fan opt-in notification types out
    across upstreams and merge them back onto one downstream stream.

### Tier 3 — Structural

11. **MRTR bridging** (see §4). The single largest work item.
12. **Extensions capability negotiation.** New multiplexing dimension: Plug must
    merge `extensions` maps across upstreams and present a coherent union
    downstream, with per-extension fallback when an upstream lacks support.
13. **Tasks extension migration.** Plug's current implementation targets the
    removed core shape. `tasks/result` → polling `tasks/get`; `tasks/list` is
    **removed** (`plug/src/ipc_proxy.rs:969` depends on it); new `tasks/update`;
    unsolicited task handles.
14. **Stateless downstream HTTP.** Delete session minting, GET stream, DELETE,
    `Last-Event-ID`; answer GET/DELETE with `405` for modern clients while keeping
    the legacy paths behind era detection.

### Tier 4 — Opportunistic

15. **OpenTelemetry `_meta` trace propagation.** Plug is a proxy; propagating
    `traceparent`/`tracestate`/`baggage` end-to-end is a natural fit and a real
    observability win across a 20-server fan-out.
16. **JSON Schema 2020-12 loosening.** Audit Plug's tool-schema handling and
    filtering for anything that assumes the old restricted subset, plus the new
    `$ref` resolution and composition-keyword bounds.
17. **MCP Apps (`io.modelcontextprotocol/ui`) pass-through.** Claude already
    renders these. Plug should not be the thing that blocks them.

### Explicitly do *not* invest here

Roots, Sampling, and Logging are deprecated. Plug's roots union-cache
(~172 refs) and logging forwarding keep working for ≥12 months. Maintain, don't
extend. Same for HTTP+SSE upstream fallback — keep it working, it is now formally
on a removal clock.

---

## 6. Suggested Sequencing

**Phase 0 — Decide.** Confirm Plug commits to dual-era rather than a hard cutover.
Everything below assumes yes.

**Phase 1 — Tier 1 quick wins.** OAuth `iss`, `application_type`, issuer binding
audit, deterministic ordering, error code. No SDK bump. Independently shippable.

**Phase 2 — `rmcp` 3.1.0 upgrade.** Mechanical but wide. Keep the wire revision at
`2025-11-25` throughout so the upgrade is separable from protocol adoption — the
same discipline the 2.2.0 upgrade used.

**Phase 3 — Modern read path.** `server/discover`, header metadata, `ttlMs`/
`cacheScope`, era detection on both faces. At the end of this phase Plug can serve
a modern client for everything that isn't MRTR or subscriptions.

**Phase 4 — Streams and state.** `subscriptions/listen`; retire session/GET/DELETE
for modern clients.

**Phase 5 — MRTR.** Both bridging directions, `requestState` crypto, parked-request
correlation. Gate flipping `protocol.rs` to accept `2026-07-28` on this landing.

**Phase 6 — Extensions.** Tasks migration, extension capability merging, MCP Apps.

---

## 7. Risks

- **`protocol.rs` rejection is now user-visible.** Anthropic is rolling `2026-07-28`
  out across Claude products. When a Claude client starts requesting it, Plug
  returns `invalid_params` and the connection fails. Whether Claude clients
  currently fall back to `2025-11-25` is **unverified and should be tested first** —
  it determines whether this is a countdown or an emergency.
- **`requestState` is security-critical.** Plug would be minting integrity-protected
  state that round-trips through a potentially hostile client. Get HMAC/AEAD,
  principal binding, TTL, and request-digest right, or don't ship it.
- **Header/body validation is mandatory, not advisory.** As an intermediary Plug
  must decode base64-sentinel values before comparing, and must forward unrecognized
  `Mcp-Param-*` headers untouched.
- **`tasks/list` removal breaks existing code** (`plug/src/ipc_proxy.rs:969`).
- **Test suite scale.** `plug-core/tests/integration_tests.rs` hardcodes
  `2025-11-25` in ~20 places and `Mcp-Session-Id` in ~6. Dual-era means these become
  legacy-era tests needing a modern-era sibling — roughly a doubling of protocol
  test surface.
- **rmcp 3.x is 6 days old** as of this writing (3.1.0, 2026-07-31). It is marked
  stable, but Plug pins exact versions for good reason. Expect to be an early adopter.

---

## 8. Open Questions

1. Do current Claude clients negotiate down to `2025-11-25` when Plug rejects
   `2026-07-28`, or do they hard-fail? **Test this before anything else** — it sets
   the urgency for the whole program.
2. Does `rmcp` 3.x provide dual-era server support in one process, or must Plug
   implement era detection and legacy handling itself?
3. Does CIMD already exist in Plug's OAuth (`docs/MCP-SPEC.md:18` lists it under
   2025-11-25), or is it documented-but-unimplemented?
4. Enterprise Managed Auth — relevant to Plug's posture as a personal tool, or
   explicitly out of scope per `docs/VISION.md`?

---

## References

- [MCP 2026-07-28 changelog](https://modelcontextprotocol.io/specification/2026-07-28/changelog)
- [MRTR pattern](https://modelcontextprotocol.io/specification/2026-07-28/basic/patterns/mrtr)
- [Streamable HTTP](https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/streamable-http)
- [Versioning & compatibility matrix](https://modelcontextprotocol.io/specification/2026-07-28/basic/versioning)
- [server/discover](https://modelcontextprotocol.io/specification/2026-07-28/server/discover)
- [Feature lifecycle & deprecation policy](https://modelcontextprotocol.io/community/feature-lifecycle)
- [MCP blog post](https://blog.modelcontextprotocol.io/posts/2026-07-28/)
- [Anthropic adoption post](https://claude.com/blog/bringing-mcp-2026-07-28-to-claude)
- [rmcp on crates.io](https://crates.io/crates/rmcp) — 3.1.0, 2026-07-31
