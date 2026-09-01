# Hard Cutover Compatibility Review — MCP 2026-07-28

**Date**: 2026-08-03
**Question**: Can Plug become a pure MCP 2026-07-28 implementation today?
**Verdict**: **B — Remain dual protocol.** Confidence: **High.**
**Scope**: Evidence-based review for maintainers deciding whether to delete a compatibility layer. No code changed.

---

## 0. Verdict

A hard cutover today would break **every MCP client observed connecting to Plug**
and **every upstream server Plug is configured against**. This is not a
forecast — it is measured, from Plug's own logs, from the installed client
binaries on this machine, and from live probes of production servers.

Three independent lines of evidence converge:

1. **Every installed client tops out at `2025-11-25`.** Claude Code 2.1.220,
   Claude Desktop 1.24012.11, Cursor 3.14.7, and Codex CLI 0.146.0 — verified by
   binary/bundle inspection. None contains `2026-07-28` protocol support.
2. **Every determinable upstream is legacy.** Plug's logs show 15 upstream
   negotiations across 2026-08-02/03: `2025-11-25` ×13, `2025-06-18` ×1,
   `2024-11-05` ×1. **Zero** `2026-07-28`.
3. **The reference implementations did not cut over.** The official TypeScript
   SDK v2, the Rust SDK 3.x, and Cloudflare's already-migrated production server
   are all **dual-era**. Nobody who has adopted the new spec has deleted the old one.

The third point is the one that should settle internal debate. A hard cutover
would mean Plug adopting a stricter posture than the spec authors' own reference
SDKs — while sitting in a strictly worse position to afford it, because Plug is a
proxy that must satisfy clients and servers it does not control on both faces.

---

## 1. Evidence Standards Used

- **Verified** — direct observation: file contents, binary strings, live HTTP
  response, log record. Reproducible command given.
- **Inferred** — reasoning from a verified fact (e.g. SDK version bounds client support).
- **UNKNOWN** — could not verify. Stated as such, never guessed.

Probe method for servers (spec §Backward Compatibility): POST `server/discover`
with `MCP-Protocol-Version: 2026-07-28` and required `_meta`. Servers **MUST**
implement `server/discover`; a modern server returns `DiscoverResult`, a legacy
server does not.

---

## 2. Client Compatibility Matrix

| Client | Version tested | 2026-07-28 | 2025-11-25 | Pure-2026 server works? | Confidence | Evidence |
|---|---|---|---|---|---|---|
| **Claude Code** | 2.1.220 (installed) | **No** | Yes | **No** | High | `strings` on binary → `2025-11-25, 2025-06-18, 2025-03-26, 2024-11-05, 2024-10-07`; no `2026-07-28`. Set matches legacy SDK `SUPPORTED_PROTOCOL_VERSIONS` exactly |
| **Claude Desktop** | 1.24012.11 | **No** | Yes | **No** | High | `strings` on `app.asar` → same five legacy versions, `2025-11-25` most frequent; no `2026-07-28` |
| **Cursor** | 3.14.7 | **No** | Yes | **No** | **Very High** | Bundles `@modelcontextprotocol/sdk@1.25.1` at `Contents/Resources/app/node_modules/`; `LATEST_PROTOCOL_VERSION='2025-11-25'`, explicit `SUPPORTED_PROTOCOL_VERSIONS` list. A second internal path declares `LATEST_PROTOCOL_VERSION="2024-11-05"` |
| **Codex CLI** | 0.146.0 (installed) | **No** | Yes | **No** | High | `codex-rs/Cargo.toml` @ tag `rust-v0.146.0` pins `rmcp = "1.8.0"` (released 2026-06-23, predates spec) |
| Codex CLI | `main` (unreleased) | Likely | Yes | Likely | Medium | `main` `Cargo.toml` pins `rmcp = "=3.0.0"` — dual-era. Not yet in a release Rob runs |
| Goose | `main` | Likely | Yes | Likely | Medium | `Cargo.toml` → `rmcp = "3.0.0"`. Release-tag status unverified |
| Roo Code | `main` | **No** | Partial | **No** | High | `src/package.json` → `@modelcontextprotocol/sdk: "1.12.0"` |
| Continue.dev | `main` | **No** | Yes | **No** | High | `core/package.json` → `@modelcontextprotocol/sdk: "^1.25.2"` |
| Gemini CLI | `main` | **No** | Yes | **No** | High | `packages/core/package.json` → `@modelcontextprotocol/sdk: "1.23.0"` |
| VS Code | `main` | **No** | UNKNOWN | **No** | Medium | `src/vs/workbench/contrib/mcp/common/mcpTypes.ts` contains only `2024-11-05`, `2025-03-26`. Other files not exhaustively searched |
| Zed | `main` | **UNKNOWN** | UNKNOWN | UNKNOWN | — | Uses own `context_server` crate v0.1.0, not `rmcp`. Not inspected further |
| Cline | — | **UNKNOWN** | UNKNOWN | UNKNOWN | — | Could not resolve `package.json` path in repo |
| Windsurf | — | **UNKNOWN** | UNKNOWN | UNKNOWN | — | Closed source; repo not resolvable |
| ChatGPT | — | **UNKNOWN** | UNKNOWN | UNKNOWN | — | Closed, server-side connector infra. Not probed |

**Hard bound (Inferred, high confidence):** any client depending on
`@modelcontextprotocol/sdk` at **any published version** cannot speak
`2026-07-28`. Verified from the published tarball of 1.30.0 — the current
`latest` dist-tag, published 2026-07-27:

```
LATEST_PROTOCOL_VERSION = '2025-11-25'
SUPPORTED_PROTOCOL_VERSIONS = [LATEST_PROTOCOL_VERSION, '2025-06-18', '2025-03-26', '2024-11-05', '2024-10-07']
```

This bounds Cursor, Roo Code, Continue, Gemini CLI, Claude Desktop, and any other
TypeScript client, regardless of how recently they bumped.

### 2.1 Empirically observed clients on this deployment

From `~/Library/Logs/plug/plug.log.2026-08-0{2,3}` (Verified):

| Client identity | Connections |
|---|---|
| `codex-mcp-client` | 75 |
| `mcp` (generic) | 15 |
| `cursor-vscode` | 4 |
| `Cursor` | 3 |
| `claude-code` | 3 |

Occurrences of `2026-07-28` anywhere in two days of logs: **0**.

A hard cutover breaks all five identities on the day it ships.

---

## 3. The npm Packaging Discontinuity

This is the most under-appreciated finding and it materially changes the adoption
timeline.

| Package | `latest` | Published | Protocol versions |
|---|---|---|---|
| `@modelcontextprotocol/sdk` | **1.30.0** | 2026-07-27 | 2024-10-07 → 2025-11-25. **No 2026-07-28** |
| `@modelcontextprotocol/core` | 2.0.0 | 2026-07-27 | 2024-10-07 → **2026-07-28** (all six) |
| `@modelcontextprotocol/server` | 2.0.0 | 2026-07-27 | — |
| `@modelcontextprotocol/client` | 2.0.0 | 2026-07-27 | — |

The v2 line ships under **new package names**. `@modelcontextprotocol/sdk@latest`
still resolves to a legacy-only SDK. Adopting 2026-07-28 in TypeScript is not a
version bump — it is a dependency migration to differently-named packages with a
consolidated schema module and changed API surface.

**Inference (high confidence):** TypeScript client adoption will be materially
slower than a normal SDK release, because maintainers must change what they
depend on, not just the constraint. Since most major MCP clients are TypeScript,
this bounds ecosystem readiness for months, not weeks.

**Verified:** `@modelcontextprotocol/core@2.0.0` contains all six version strings
including `2026-07-28` — the new SDK is **dual-era**, not modern-only.

---

## 4. Server-Side Reality

| Server | 2026-07-28 | Evidence | Confidence |
|---|---|---|---|
| **Cloudflare docs MCP** | **Yes — and dual-era** | `server/discover` → `{"supportedVersions":["2026-07-28"],...,"ttlMs":...}`. Legacy `initialize` → `{"protocolVersion":"2025-11-25","serverInfo":{"name":"docs-ai-search","version":"0.4.10"}}` | Verified |
| exa | No | `400` — *"Unsupported protocol version: 2026-07-28 (supported: 2025-11-25, 2025-06-18, 2025-03-26, 2024-11-05, 2024-10-07)"* | Verified |
| DeepWiki | No | `400` — supported: `2024-11-05, 2025-03-26, 2025-06-18, 2025-11-25` | Verified |
| context7 | No | `400` — *"No valid session ID provided"*; requires the removed `Mcp-Session-Id` | Verified |
| svelte | No | `200` with `-32601 Method not found` for `server/discover` (mandatory for modern servers) | Verified |
| Notion, Todoist, GitHub, Sentry, Linear, Stripe | UNKNOWN | `401` — auth required, not probed authenticated | — |

**The Cloudflare result is the single most instructive data point in this
review.** Cloudflare has fully migrated — proper `DiscoverResult`, cache hints,
the works — and *still answers legacy `initialize`*. The most advanced adopter in
the ecosystem chose dual-era. `supportedVersions` advertising only `2026-07-28`
while still serving legacy `initialize` also shows that field is not a reliable
signal of what a server will actually accept.

### 4.1 Plug's own upstream fleet

From Plug's logs, `"Service initialized as client"` records (Verified):

| Negotiated version | Count |
|---|---|
| `2025-11-25` | 13 |
| `2025-06-18` | 1 |
| `2024-11-05` | 1 |
| `2026-07-28` | **0** |

12 enabled upstreams; 6 are third-party remote SaaS (context7, exa, krisp,
notion, svelte, todoist) that Rob cannot upgrade or redeploy. Per the spec's
compatibility matrix, **Modern client → Legacy server = Fails**.

---

## 5. Breaking Changes vs. Plug's Actual Dependencies

| Change | Why it changed | Plug depends on it? | Hard cutover removes it? |
|---|---|---|---|
| `initialize` / `notifications/initialized` removed | Statelessness; per-request `_meta` | **Yes** — 13 files | Yes. Every observed client *requires* it |
| Protocol sessions + `Mcp-Session-Id` | Horizontal scaling without sticky routing | **Yes** — 20 files; `SESSION_ID_HEADER` in `http/server.rs:37` | Yes. context7 upstream *requires* it |
| HTTP GET/DELETE lifecycle | Replaced by `subscriptions/listen` | **Yes** — `http/server.rs:413` routes all three | Yes |
| SSE replay (`Last-Event-ID`) | Simplifies stateless serving | **Yes** — 5 files; `transport/sse_client.rs:303-435` | Yes. Loses reconnect resilience to legacy upstreams |
| `resources/subscribe`/`unsubscribe` | Folded into `subscriptions/listen` | **Yes** — 16 files | Yes |
| Server-initiated requests → MRTR | No held-open bidirectional streams | **Yes** — 10 files (elicitation/sampling) | Replaced, not removed — and requires new `requestState` crypto |
| Roots | Deprecated (≥12mo) | **Yes** — 10 files, union cache | Deprecated, still functional |
| Logging / `logging/setLevel` | Per-request `_meta` logLevel | **Yes** — 15 files | Yes |
| `ping` removed | Redundant under stateless | Minimal (1 ref) | Yes |
| Tasks redesign | Moved to extension; `tasks/list` **removed** | **Yes** — 4 files; `ipc_proxy.rs:969` calls `tasks/list` | Breaks — must be rewritten |
| `server/discover` | Replaces initialize for capability discovery | No — **missing** | New work either way |
| `resultType` on all results | MRTR discrimination | No — **missing** | New work either way |
| Extension negotiation | Versioned optional features | No — **missing** | New work either way |
| Cache hints (`ttlMs`/`cacheScope`) | Reduce polling; prompt-cache stability | No — **missing** | New work either way (pure win) |
| Header metadata | Intermediary routing without body parsing | **Partial** — `mcp_http_headers.rs` exists | Extend, not create |

**Reading**: the top block is what a hard cutover *deletes* and every observed
client still needs. The bottom block is new work that must be done **regardless**
of whether Plug goes dual-era or cuts over. A hard cutover therefore does not
avoid the new work — it only adds breakage on top of it.

---

## 6. Internal Blast Radius

Measured over `plug-core/src` + `plug/src` (65,363 lines):

| Subsystem | Files touched | Migration size | Notes |
|---|---|---|---|
| Session store / IDs | 20 | **Medium** | Deletes cleanly for modern; must persist for legacy |
| HTTP server (routes, GET/DELETE, SSE) | ~6 | **Medium** | `http/server.rs` is 4,297 lines |
| Subscriptions | 16 | **Architectural** | `resources/subscribe` → `subscriptions/listen` reshapes fan-out across ~12 upstreams |
| Reverse requests (elicitation/sampling) | 10 | **Architectural** | MRTR bridging; see §6.1 |
| Roots | 10 | Small (deprecated) | Maintain, don't extend |
| Logging forwarding | 15 | **Medium** | Per-request `logLevel` gating changes emission rules |
| Task system | 4 + `ipc_proxy.rs` | **Architectural** | `tasks/result`→polling, `tasks/list` deleted |
| SSE client / reconnect | 5 | **Medium** | Resumability must stay for legacy upstreams |
| OAuth | `oauth.rs` 2,109 lines + `downstream_oauth/` | **Medium** | Independent of cutover; see §6.2 |
| Daemon / IPC | `ipc.rs`, `daemon` | **Medium** | IPC carries protocol + client identity today |
| Routing / proxy core | `proxy/mod.rs` 2,630 lines | **Medium** | `resultType` handling on every result |
| Tracing | — | Small | OTel `_meta` propagation is additive |
| Tests | `integration_tests.rs` | **Medium** | 17 hardcoded `2025-11-25`; dual-era ≈ doubles protocol test surface |

### 6.1 The MRTR asymmetry — why cutover is *more* expensive

Counterintuitive but important:

- **Today** (legacy client + legacy upstream): server-initiated
  `elicitation/create` passes straight through. This path works and is exercised.
- **After a downstream-only cutover** (modern clients + legacy upstreams): *every*
  elicitation/sampling/roots interaction needs the hard bridge — park the in-flight
  upstream request, mint integrity-protected `requestState` (HMAC/AEAD, principal-bound,
  TTL, request-digest per spec §MRTR security requirements), correlate a retry arriving
  under a **different JSON-RPC id**.

A hard cutover front-loads the single hardest subsystem in the program **and**
removes the working fallback. Dual-era lets Plug ship `server/discover`, header
metadata, and cache hints first and defer MRTR until a modern client actually
appears.

### 6.2 Hidden blockers checked

| Area | Requires legacy? | Evidence |
|---|---|---|
| OAuth | **No** | Auth hardening (RFC 9207 `iss`, `application_type`, issuer binding) is orthogonal to the wire revision. Ship independently |
| Claude integration | **Yes** | Claude Code 2.1.220 + Desktop 1.24012.11 verified legacy-only |
| Cursor integration | **Yes** | Bundled SDK 1.25.1 verified |
| Codex integration | **Yes** | Installed 0.146.0 pins rmcp 1.8.0 |
| stdio MCP | **Yes** | 4 enabled stdio upstreams; era probe needed per spec |
| Remote MCP | **Yes** | 8 HTTP upstreams; 4 verified legacy, 4 UNKNOWN |
| Long-running ops / Tasks | **Yes** | `tasks/list` removed; `ipc_proxy.rs:969` depends on it |
| Streaming | **Yes** | Loss of `Last-Event-ID` degrades legacy upstream reconnect |
| MCP Apps | UNKNOWN | Extension; not currently implemented in Plug |
| ChatGPT | UNKNOWN | Not verified either way |

---

## 7. RMCP 3.x Migration

**Verified from release notes** (`gh api repos/modelcontextprotocol/rust-sdk/releases`):

rmcp 3.0.0 (2026-07-28), 3.0.1 (07-29), 3.1.0 (07-31). Migration guide:
[rust-sdk discussion #969](https://github.com/modelcontextprotocol/rust-sdk/discussions/969).

**Breaking changes:**
- Sessionless Streamable HTTP; fresh handler per request — persistent state must live outside the handler
- `stateful_mode` → **`legacy_session_mode`**, now controlling only older protocol versions
- Stateless lifecycle: clients opt in via `serve_with_lifecycle` + `ClientLifecycleMode::Discover | Auto`
- `subscriptions/listen` replaces GET stream and `resources/subscribe`
- `ServerResult` matches must handle `InputRequiredResult`
- `_meta` split into `MetaObject` / `RequestMetaObject` / `NotificationMetaObject`
- OAuth consolidated around `AuthorizationRequest`
- Deprecated v3 APIs removed; `server_info` removed from `DiscoverResult`
- MSRV 1.88 — **Plug already at 1.88.0**, no blocker

**The decisive fact:** rmcp 3.x is explicitly **dual-era**.

> "the existing `serve()` path remains available for legacy initialization"

Verified corroboration: rmcp 3.1.0's source carries `2024-11-05, 2025-03-26,
2025-06-18, 2025-11-25` *and* `2026-07-28`; 3.1.0 fixed *"honor
supported_protocol_versions when negotiating initialize"*; beta.4 fixed *"omit
resultType for legacy protocol sessions"*. These are dual-era maintenance fixes.

**Implication:** `ClientLifecycleMode::Auto` gives Plug upstream era detection and
`legacy_session_mode` gives downstream legacy serving — **largely for free**. Dual-era
is close to the SDK's default posture. A hard cutover means *deliberately disabling*
capability rmcp already ships.

**Churn signal (Medium confidence):** three releases in four days, with fixes to
protocol metadata validation, MRTR result decoding, and initialize negotiation.
That is a normal shape for a just-shipped major, but it argues for tracking 3.x
briefly before pinning, not for pinning 3.0.0 today.

---

## 8. Ecosystem Adoption

| Party | Status | Confidence | Evidence |
|---|---|---|---|
| **Cloudflare** | **Migrated, dual-era** | Verified | `docs.mcp.cloudflare.com/mcp` serves both eras |
| **Anthropic (SDKs)** | Shipped, dual-era | Verified | TS `core@2.0.0` carries all six versions |
| **Anthropic (clients)** | **Not shipped** | Verified | Claude Code 2.1.220 + Desktop 1.24012.11 legacy-only |
| **OpenAI (Codex)** | In progress | Verified | `main` on rmcp 3.0.0; released 0.146.0 on rmcp 1.8.0 |
| Block (Goose) | In progress | Medium | `main` on rmcp 3.0.0; release status unverified |
| Microsoft (VS Code) | Legacy | Medium | `mcpTypes.ts` shows only 2024-11-05 / 2025-03-26 |
| GitHub MCP server | UNKNOWN | — | `401`, requires auth |
| Cursor, Continue, Roo, Gemini CLI | Legacy | High | Pinned legacy SDK versions |

**Pattern:** *server-side infrastructure* providers are migrating first
(Cloudflare — the party that benefits most from stateless/serverless). *Clients*
have not. That asymmetry is exactly backwards from what a hard cutover needs:
Plug's downstream face serves clients.

Notably: **zero verified instances of anyone deleting legacy support.** Every
adopter found is dual-era.

---

## 9. Recommendation

### **B — Remain dual protocol.**

**Clients that still require legacy (Verified, blocking):**
Claude Code 2.1.220 · Claude Desktop 1.24012.11 · Cursor 3.14.7 · Codex CLI 0.146.0 — i.e. **100% of clients observed connecting to this deployment.**

**Technical constraints that still require legacy (Verified, blocking):**
- 6 third-party remote upstreams Rob cannot upgrade; 4 verified legacy (exa, context7, svelte, DeepWiki-class), 4 UNKNOWN
- context7 actively *requires* `Mcp-Session-Id`, which the new spec deletes
- `tasks/list` removal breaks `plug/src/ipc_proxy.rs:969`
- The npm packaging discontinuity bounds TypeScript client adoption for months

**Proposed posture:**

1. **Ship Tier-1 auth hardening now.** Independent of the wire revision. `iss`
   validation is the priority — per prior verification, rmcp records
   `require_issuer` from AS metadata, so an AS advertising RFC 9207 fails login today.
2. **Upgrade to rmcp 3.x, hold the wire revision at `2025-11-25`.** Same discipline
   as the 2.2.0 upgrade. Consider tracking 3.x a few weeks before pinning, given
   three releases in four days.
3. **Adopt dual-era via rmcp's own mechanisms** — `ClientLifecycleMode::Auto`
   upstream, `legacy_session_mode` downstream. Do not hand-roll a compatibility layer.
4. **Close the HTTP guard hole deliberately.** `ensure_supported_downstream_protocol`
   is wired into stdio and IPC but not the HTTP initialize path, so an HTTP client
   requesting `2026-07-28` silently receives `2025-11-25`. Survivable for a dual-era
   client, but it should be a decision, not an accident.
5. **Defer MRTR** until a modern client is actually observed. It is the most
   expensive subsystem and currently has zero consumers.

### Re-evaluation triggers

Revisit a cutover when **all** hold:

- [ ] Claude Code and Claude Desktop ship builds containing `2026-07-28`
- [ ] `@modelcontextprotocol/sdk` `latest` points at the v2 line, **or** major TS clients migrate to `@modelcontextprotocol/{core,client}`
- [ ] ≥80% of Plug's configured upstreams negotiate `2026-07-28`
- [ ] Plug's logs show modern downstream clients for a sustained period

**Cheap standing instrumentation:** Plug does not currently log the *downstream*
negotiated protocol version (only `client_info`, often `None` at registration).
Adding that single log field turns every future re-evaluation into a one-line
query instead of a research project. Recommended regardless of the decision.

---

## 10. Confidence and Gaps

**High confidence:** installed-client versions; SDK version bounds; upstream
negotiation history; rmcp dual-era support; npm packaging split; Cloudflare dual-era.

**Medium confidence:** VS Code (partial file search); Goose/Codex `main` status
(unreleased); rmcp 3.x stability.

**Explicit UNKNOWNs — not guessed:**
- ChatGPT MCP client protocol support
- Zed (own `context_server` crate, not inspected in depth)
- Cline, Windsurf
- Notion, Todoist, GitHub, Sentry, Linear, Stripe (auth-gated, not probed authenticated)
- Whether Claude clients *negotiate down* or *hard-fail* against a modern-only server

None of these gaps would change the recommendation: the blocking evidence comes
from clients and upstreams that **are** verified.

**Method limitation:** binary `strings` inspection proves a version string is
*absent* (strong negative evidence) but presence alone is ambiguous — the Codex
binary contains `2026-07-28` while pinning pre-spec rmcp 1.8.0, because model-release
dates share the date format. Negative findings above are load-bearing; no positive
claim rests on `strings` alone.

---

## Reproduction

```bash
# Client bundles
strings /Users/robdezendorf/.local/share/claude/versions/2.1.220 | grep -oE '"20[0-9]{2}-[0-9]{2}-[0-9]{2}"' | sort -u
strings /Applications/Claude.app/Contents/Resources/app.asar | grep -oE '"20[0-9]{2}-[0-9]{2}-[0-9]{2}"' | sort | uniq -c
grep -oE "SUPPORTED_PROTOCOL_VERSIONS *= *\[[^]]*\]" \
  /Applications/Cursor.app/Contents/Resources/app/node_modules/@modelcontextprotocol/sdk/dist/esm/types.js

# SDK bounds
curl -s https://unpkg.com/@modelcontextprotocol/sdk@1.30.0/dist/esm/types.js | grep -oE "LATEST_PROTOCOL_VERSION *= *'[^']+'"
curl -s https://unpkg.com/@modelcontextprotocol/core@2.0.0/dist/auth-CUe6YdwF.mjs | grep -oE '20[0-9]{2}-[0-9]{2}-[0-9]{2}' | sort -u

# Server probe
curl -s -X POST https://docs.mcp.cloudflare.com/mcp -H 'Content-Type: application/json' \
  -H 'Accept: application/json, text/event-stream' -H 'MCP-Protocol-Version: 2026-07-28' \
  -H 'Mcp-Method: server/discover' \
  -d '{"jsonrpc":"2.0","id":1,"method":"server/discover","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientInfo":{"name":"probe","version":"1.0"},"io.modelcontextprotocol/clientCapabilities":{}}}}'

# Upstream negotiation history
grep -h 'protocol_version' ~/Library/Logs/plug/plug.log.2026-08-0* | grep -oE 'ProtocolVersion..[0-9-]+' | sort | uniq -c
```

## References

- [MCP 2026-07-28 changelog](https://modelcontextprotocol.io/specification/2026-07-28/changelog) · [versioning & compat matrix](https://modelcontextprotocol.io/specification/2026-07-28/basic/versioning) · [MRTR](https://modelcontextprotocol.io/specification/2026-07-28/basic/patterns/mrtr) · [Streamable HTTP](https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/streamable-http)
- [RMCP 3.0 migration guide](https://github.com/modelcontextprotocol/rust-sdk/discussions/969)
- Companion: `docs/research/2026-08-03-mcp-2026-07-28-modernization-survey.md`
