# Can plug become a pure MCP 2026-07-28 implementation today?

**Date:** 2026-08-03 · **Author:** claude-fable (six parallel research agents + main-thread verification) · **Repo state:** `main` @ `e0aaff7`

**Verdict: B — remain dual protocol.** A hard cutover today would break **100% of observed downstream traffic** (101/101 sessions in the last two days used the legacy `initialize` handshake) and **all 13 configured upstreams** (every one negotiated ≤ 2025-11-25). Zero of fourteen surveyed clients can connect to a pure-2026 server in their default shipped configuration. Zero production servers anywhere were found running 2026-07-28-only. This is not a close call; the evidence is unanimous across five independent streams.

Evidence labels used throughout: **VERIFIED** (primary source: binary/source inspection, official changelog, spec text, plug code at HEAD), **REPORTED** (secondary source), **INFERRED** (labeled), **UNKNOWN**.

---

## 1. Client compatibility matrix

"Pure-2026 works?" = can this client, as shipped and default-configured today, connect to a server that rejects `initialize` and has no sessions.

| Client | Latest version | 2026-07-28 support | 2025-11-25 support | Pure-2026 works? | Confidence | Key evidence |
|---|---|---|---|---|---|---|
| **Claude Code** | 2.1.220 | Not shipped (announced "soon") | Yes | **No** | High | VERIFIED binary grep: `LATEST="2025-11-25"`, supported list ends there; zero `2026-07-28` strings |
| **Claude Desktop** | 1.24012.11 | Not shipped (announced "soon") | Yes (TS SDK v1) | **No** | High | VERIFIED app.asar grep: SDK v1 constants + `initialize` handshake code |
| **ChatGPT** (connectors) | desktop 26.727.51351 | No signal; backend UNKNOWN | Likely (undocumented) | **No evidence of yes** | Medium | OpenAI docs mention only transports, never a protocol revision; bundled Codex binary knows the version *string* but has zero `server/discover` strings |
| **Cursor** | 3.14.7 | Not shipped | Yes (bundles SDK 1.25.1) | **No** | High | VERIFIED bundled `node_modules` inspection |
| **VS Code** (Copilot MCP) | 1.131 (Jul 29) | Not shipped — **not even on `main`** | Yes | **No** | High | VERIFIED source: `modelContextProtocol.ts` `LATEST_PROTOCOL_VERSION = "2025-11-25"`, own initialize-based client |
| **Windsurf** (→ Devin Desktop) | 3.6.27 (Aug 1) | No signal | INFERRED yes | Unknown → assume no | Low-med | Closed source; changelog only. Weakest row |
| **Zed** | 1.13.2 (Aug 2) | Not shipped, not on `main` | Yes | **No** | High | VERIFIED source: `context_server/src/types.rs` |
| **Codex CLI** (OpenAI) | 0.146.0 | **On `main`, behind off-by-default flag** `mcp_2026_07_28` (Stage::UnderDevelopment) | Yes (default) | Yes with flag; **No by default** | High | VERIFIED source (`protocol_mode.rs`, `features/src/lib.rs`) + local binary grep (7 hits of `2026-07-28`) |
| **Goose** (Block) | 1.45.0 | No (rmcp 3.0.0 bumped on `main` but legacy `serve()` lifecycle; zero `2026-07-28` in repo) | Yes | **No** | High | VERIFIED Cargo.toml + code search |
| **Gemini CLI** | 0.53.1 | No signal (SDK 1.23.0) | Yes | **No** | High | VERIFIED package.json |
| **Continue.dev** | 2.1.0 | No signal (SDK ^1.25.2) | Yes | **No** | High | VERIFIED package.json |
| **Cline** | 4.1.3 | No signal (SDK ^1.25.1 / ^1.29.0) | Yes | **No** | High | VERIFIED package.json |
| **Roo Code** | 3.54.0 | No signal (SDK **1.12.0**, dep bumps stalled) | Yes | **No** | High | VERIFIED package.json; abandoned renovate PR #9753 |
| **LibreChat** | 0.8.7 | No signal (SDK ^1.29.0) | Yes | **No** | High | VERIFIED package.json |

**SDK layer (what clients will inherit):** all five Tier-1 SDKs shipped 2026-07-28 support around July 27–28 (TS split-packages 2.0.0, Python 2.0.0, Go 1.7.0, C# 2.0.0, rmcp 3.0.0). Critically, the TS SDK v1 line — what **every** surveyed TS client actually pins — tops out at 2025-11-25 even in its final 1.30.0 release, and TS SDK **v2 defaults to `versionNegotiation: 'legacy'`** (no discover probe). Python v2 defaults to `auto`; rmcp requires explicit `serve_with_lifecycle` opt-in. So even after clients bump SDKs, 2026-07-28 on the wire remains an explicit opt-in in the two ecosystems that matter most here (TS, Rust). VERIFIED via npm tarball unpacking and SDK docs.

## 2. Local empirical evidence (this machine, VERIFIED)

- **101/101 downstream sessions** across `plug.log.2026-08-02/03` used `initialize` — verified structurally (both log sites fire only from initialize handlers). Clients: codex-mcp-client ×75, "mcp" ×15 (identity unknown), cursor-vscode ×4, Cursor ×3+1, claude-code ×3.
- **All upstreams legacy:** Notion, Krisp, Context7, Exa, Figma, google_workspace, iMCP, iMessage Max, Slack → 2025-11-25; Svelte → 2025-06-18; Oura → **2024-11-05**.
- **Zero occurrences** of `2026-07-28`, `server/discover`, or `UnsupportedProtocolVersion` in any plug log.
- Installed-client binaries: only Codex CLI carries the 2026-07-28 constant (flag-gated); Claude Code, Claude Desktop, Cursor, OpenCode all verified legacy.

## 3. Breaking protocol changes → plug dependency

Sources: spec changelog (modelcontextprotocol.io/specification/2026-07-28/changelog), MCP release blog. Plug facts verified at HEAD.

| Change (SEP) | What/why | Plug depends today? | Hard cut removes it? |
|---|---|---|---|
| `initialize`/`initialized` removed; per-request `_meta` (`protocolVersion`/`clientCapabilities`/`clientInfo`) (SEP-2575) | Enables any-instance request handling | **Yes — load-bearing on all 3 downstream transports + upstream connect-at-boot** (`server/mod.rs:312`, `proxy/handler.rs:209`, `http/server.rs:1409`, `ipc_proxy.rs:675`) | Deleted; replaced by `_meta` reading + `server/discover` |
| Sessions / `Mcp-Session-Id` removed (SEP-2567) | Serverless/LB deployability | **Yes** — `StatefulSessionStore` (581 LOC) + 5 side-tables; strictly session-required HTTP | Deleted outright |
| HTTP GET stream + DELETE lifecycle removed; `subscriptions/listen` replaces GET + `resources/subscribe`/`unsubscribe` (SEP-2575) | One opt-in notification stream | **Yes** — GET/DELETE handlers, 8-step teardown, subscription registry (~1,300 LOC) both sides | GET/DELETE deleted; registry **rebuilt** on stream lifecycle. `subscriptions/listen` is transport-neutral (long-lived request; response stream carries notifications) — resolves the stdio question |
| SSE resumability (`Last-Event-ID`) removed (SEP-2575) | Broken stream = re-issue request | **Yes** — replay buffer, replay keys, 32-event pending queue | Deleted |
| `server/discover` mandatory (SEP-2575) | Up-front version/capability advertisement; doubles as back-compat probe | Missing entirely | Net-new; `synthesized_capabilities_for_client` (`catalog.rs:467`) is the single choke point to source it |
| MRTR replaces server-initiated elicitation/sampling/roots; required `resultType` (SEP-2322) | No open streams needed for server→client asks | **Yes** — reverse-request bridge, ~850 LOC, 3 impls, known concurrency cliff (`proxy/mod.rs:709`) | Pure cut: deleted, replaced by stateless HMAC-sealed rewrap (~300 LOC, net negative). **Realistic cut (legacy upstreams): REWRITE** — parked-request table translating upstream blocking elicitation into downstream `input_required` retries |
| `ping`, `logging/setLevel`, `notifications/roots/list_changed` removed; per-request `_meta` log level (SEP-2575) | Statelessness | plug serves `ping`, implements global setLevel (known cross-client leak, `docs/DECISIONS.md:36`), forwards roots list_changed. Health monitor is **safe** — probes via `list_tools`, not ping (`health.rs:287`; module doc stale) | Deleted (~420 LOC logging + ~330 roots). **Feature loss:** plug's `plug.auth` token-refresh notifications lose their delivery channel on stdio/HTTP (IPC unaffected) |
| Roots/Sampling/Logging deprecated (SEP-2577, ≥12-month window) | Migration to tool params / provider APIs / OTel | Yes (all forwarded) | Optional deletion; not forced |
| Tasks → `io.modelcontextprotocol/tasks` extension: `tasks/get` polling, `tasks/update`, `tasks/listen`; `tasks/list` and blocking `tasks/result` removed (SEP-2663) | Long-running work without core changes | **Yes** — first-class tasks on all 3 transports (~1,800 LOC); `tasks/list` wired incl. CLI over IPC | MEDIUM rework; store/cancellation machinery survives. Note: `tasks/listen` exists in the spec/blog but **rmcp 3.1 has not implemented it** (polling only) |
| `ttlMs`/`cacheScope` required on list/read results (SEP-2549) | Client caching, less polling | No — all four list results built with `meta: None` (`catalog.rs:392,612,632,647`) | Must **emit** (cheap, 4 sites). Consuming upstream TTLs = first time-based trigger in a purely event-driven refresh system |
| `Mcp-Method`/`Mcp-Name` headers required; `x-mcp-header` (SEP-2243) | Gateway routing without body parsing | **Already shipped** (`mcp_http_headers.rs`; emit upstream, validate downstream) | Keep; add `x-mcp-header`; renumber HeaderMismatch −32001→−32020 |
| Auth: RFC 9207 `iss` validation MUST (SEP-2468); issuer-keyed credentials (SEP-2352); `application_type` (SEP-837); DCR deprecated → CIMD | Mix-up attack closure; credential hygiene | `iss`: **absent — live failure** against any AS advertising RFC 9207 (rmcp sets `require_issuer` from metadata but plug uses the issuer-less shim). Issuer keying: absent (keyed by config name). `application_type`: already sent (rmcp default). CIMD: downstream AS serves it; upstream client can't consume it | Orthogonal to the cut; required regardless |
| Resource-not-found −32002→−32602; error-code partitioning (−32020..−32099 reserved) | JSON-RPC alignment | Yes — plug emits legacy codes | SMALL renumbering |
| HTTP+SSE transport formally deprecated (SEP-2596) | 1-year offramp | Yes — hand-rolled SSE client (~1,300–1,400 LOC incl. tests) + auto-fallback | Deletable only if upstream face cuts — which it must not (§5) |

## 4. What breaks inside plug (sizing, verified at HEAD)

Full detail in the plug-spec-map sizing supplement; summary:

| Class | Subsystems | Impl LOC |
|---|---|---|
| REWRITE | downstream HTTP server (net deletion), stdio handler, upstream engine (only if upstream face cuts), subscriptions registry, notifications fan-out (net deletion), rmcp 2.2→3.1 bump, MRTR-under-realistic-cut | ~6,700 |
| MEDIUM | daemon/IPC message set (wire protocol itself unaffected — plug's own JSON framing), tasks extension alignment, OAuth, health/recovery | ~3,100 |
| SMALL | roots/logging/sampling deletion, legacy SSE deletion, protocol constants (5 sites → 1) | ~2,500 |

Impl total ~12–13k LOC touched (~4–5k net deleted); **test churn ~8–10k more** (≈20 hardcoded `2025-11-25` sites, ≈6 `Mcp-Session-Id` sites). Grand total **~20–23k lines, a third to half the workspace**; rough solo estimate 8–14 weeks (INFERRED, low confidence). Highest-risk single item: `proxy/tasks.rs` destructures rmcp-internal `RequestHandle` fields (doc comment says verified against rmcp **1.7.0**) — breaks semantically, possibly silently, on any bump; replace, don't port. Best single deletion: MRTR removes the daemon's head-of-line-blocking reverse-request plumbing (~220 LOC of the gnarliest code in the repo).

**The structural finding:** "full hard cutover" is two independent decisions. The **upstream** face cannot cut — plug fans out to ~21 third-party servers Rob controls none of, all verified legacy; a pure-2026 upstream client talks to zero of them. But if upstream stays legacy while downstream goes modern, plug lands in exactly the hard bridging case (legacy blocking elicitation → modern `input_required` retries), so **the hard cut doesn't avoid the hard MRTR problem — it forces it**.

## 5. Hidden blockers

1. **Both Claude paths die at handshake.** Claude Code (`plug connect`) and Claude Desktop (stdio + tunnel HTTP) verified legacy at binary level; a pure-2026 plug answers their `initialize` with method-not-found. No graceful degradation exists.
2. **ChatGPT reachability requires legacy indefinitely** — OpenAI has shipped nothing and announced nothing for 2026-07-28 (VERIFIED-ABSENT, medium-high).
3. **TS SDK v2 defaults to legacy negotiation** — client vendors bumping SDKs will not automatically start probing `server/discover`.
4. **rmcp gaps:** `tasks/listen` unimplemented (polling only, conformance "expected failures"); open P1 #1114 — macro-generated list results omit required `ttlMs`/`cacheScope`, so strict 2026 clients (TS SDK 2.0.0) reject them → **plug must stamp these itself when bridging legacy upstreams**; open P1 #1095 blocks MRTR in macro handlers; skip 3.0.x entirely (silent `InputRequiredResult` misdeserialization, fixed in 3.1.0).
5. **rmcp `Auto` fallback triggers only on `METHOD_NOT_FOUND`** — a legacy server answering `server/discover` with a timeout or malformed error fails the connection rather than falling back. With 21 mixed-vintage upstreams, expect stragglers.
6. **plug's auth-state notifications** (`plug.auth` token-refresh/logout signals) have no channel under the new logging rules on stdio/HTTP — product decision needed before any cut.
7. **OAuth `iss`**: independent of the cut, `plug auth login` fails today against any RFC-9207-conformant AS (rmcp records `require_issuer`; plug hardcodes `received_issuer = None`).
8. **Elicitation outside `tools/call`/`resources/read`/`prompts/get` has no MRTR expression** — an upstream asking for input at any other moment has no path in the new protocol.

## 6. rmcp 2.2.0 → 3.1.0 migration (VERIFIED, discussion #969 + docs.rs + release tags)

- **Dual protocol is native and per-connection**: server side, 2026-07-28 requests are always stateless regardless of `legacy_session_mode`; legacy clients get sessions simultaneously on the same endpoint. Client side, `ClientLifecycleMode::Auto{preferred_versions, legacy_version}` probes discover, falls back to initialize. Caveat: the server-side adaptivity lives in rmcp's `StreamableHttpService` — plug's hand-rolled axum server must replicate it or adopt the tower service.
- Breaking: MRTR response enums on handler traits (`.into()` migration), six protocol enums now `#[non_exhaustive]` (hits plug's routing matches), `Meta` split into three types, `Annotations::last_modified` → `Option<String>` (good for proxying), OAuth consolidated on `AuthorizationRequest` builder with reactive 401-challenge discovery (lands on plug's fresh downstream OAuth), pre-3.0 deprecated APIs removed, MSRV 1.88 (already satisfied).
- "Beta" label correction: rmcp 3.0.0 release notes do **not** call 2026 support beta; ROADMAP says Tier-1 requirements met, 100% conformance except tasks extension.
- Maintainer strategy: bump-then-adopt, seven ordered steps. Precursor for plug: **delete the crate-wide `#![allow(deprecated)]` (`plug-core/src/lib.rs:4`) while still on 2.2.0** — it currently hides the entire migration surface.

## 7. Real-world adoption (as of 2026-08-03)

- **Dual-stack everywhere, pure-2026 nowhere.** GitHub MCP server (shipped Jul 23, Redis sessions removed), Cloudflare (Agents SDK v0.20.0 day-zero; hosted servers stateless per-request but `/mcp` still accepts 2025 clients), Microsoft (C# SDK GA; ADR-0027 codifies dual-stack with no calendar cutoff), Sentry/Linear/Figma/Supabase/Netlify/PostHog/AWS AgentCore/Google Cloud (REPORTED via official MCP blog). **Zero legacy-rejecting production servers found.** No vendor has announced a 2025-11-25 sunset.
- Notion and Krisp — official remote servers — still negotiated 2025-11-25 with plug today (VERIFIED ground truth). The long tail (e.g., Glama's 67k community servers) is on pre-2026 SDKs.
- Reference client pattern (Cloudflare, all Tier-1 SDKs, spec changelog): `server/discover` probe → fall back to `initialize`.
- Deprecation policy: 12-month **minimum** window; deprecated features live through at least mid-2027; the initialize handshake itself has **no announced sunset from anyone**.

## 8. Recommendation

**B — remain dual protocol.** Hard cutover today yields a product that cannot talk to any of its observed clients or any of its configured upstreams. Not one link in plug's actual graph — 5 client apps verified at binary level, 101 logged sessions, 13 active upstreams, and the broader ecosystem — is pure-2026-ready.

**Exit criteria for revisiting A** (all must hold; check via a protocol-version census, which plug should start logging — it currently discards the client's requested version after the guard check):

1. Shipping Claude Code + Claude Desktop binaries carry 2026-07-28 **and** observed plug sessions stop sending `initialize` (census ≈100% modern for 30 days).
2. Every configured upstream answers `server/discover`.
3. ChatGPT/OpenAI connectors support the revision (or Rob explicitly drops that reachability goal).
4. rmcp closes #1114/#1095 and ships `tasks/listen`.
5. Realistically: not before the deprecation window matures (mid-2027 at the earliest).

**Sequence toward dual-stack (risk retired per unit of work):**

1. Delete `#![allow(deprecated)]` on rmcp 2.2.0; fix fallout (exposes the real migration surface).
2. OAuth `iss` validation + issuer-keyed credentials (live security gap + spec MUST, cut-independent).
3. Close the HTTP protocol-guard hole (`ensure_supported_downstream_protocol` never called on the HTTP path) + add requested-version census logging.
4. rmcp bump straight to ≥3.1.0, wire pinned at 2025-11-25 (SDK move separable from protocol adoption). Replace the `RequestHandle` destructuring with a supported API first.
5. Adopt 2026-07-28 **additively**: downstream accept both eras (rmcp's per-connection adaptive model or replicate in axum); upstream `Auto` lifecycle with pinned `legacy_version` per server.
6. MRTR bridging (legacy upstream ↔ modern downstream) last — it is the genuinely new, security-critical subsystem (HMAC-sealed `requestState`, parked-request table with TTL).

## Appendix: source index

- Spec: modelcontextprotocol.io/specification/2026-07-28/changelog · blog.modelcontextprotocol.io/posts/2026-07-28/ · claude.com/blog/bringing-mcp-2026-07-28-to-claude
- rmcp: github.com/modelcontextprotocol/rust-sdk (releases; discussion #969; ROADMAP.md; issues #1114 #1095 #1094 #1098 #1096 #1108 #1109; docs.rs/rmcp/3.1.0)
- SDKs: github.com/modelcontextprotocol/{typescript-sdk,python-sdk,go-sdk,csharp-sdk} releases · blog.modelcontextprotocol.io/posts/sdk-betas-2026-07-28/ · py.sdk.modelcontextprotocol.io/protocol-versions/
- Clients: code.claude.com/docs/en/changelog · code.visualstudio.com/updates/v1_131 · cursor.com/changelog · github.com/zed-industries/zed · github.com/openai/codex (protocol_mode.rs, features) · github.com/block/goose · per-repo package.json/Cargo.toml pins as cited in §1 · local binary greps (Claude Code 2.1.220, Claude.app 1.24012.11, Cursor.app 3.14.7, ChatGPT.app 26.727.51351, OpenCode 1.18.11)
- Ecosystem: github.blog/changelog/2026-07-23-github-mcp-server-supports-the-next-mcp-specification/ · developers.cloudflare.com/changelog (2026-07-27, 2026-07-28) · devblogs.microsoft.com/dotnet/announcing-v20-of-the-official-mcp-csharp-sdk/ · github.com/microsoft/agent-governance-toolkit ADR-0027 · developers.openai.com/api/docs/guides/tools-connectors-mcp · aws.amazon.com/blogs/machine-learning/how-agentcore-gateway-supports-the-mcp-2026-07-28-spec/ · gofastmcp.com/changelog
- Local: ~/Library/Logs/plug/plug.log.2026-08-02/-03 · installed app bundles · plug source at `main` @ e0aaff7
- Related: docs/research/2026-08-03-mcp-2026-07-28-modernization-survey.md (prior survey; three status-table corrections documented in session memory 2026-08-03)
