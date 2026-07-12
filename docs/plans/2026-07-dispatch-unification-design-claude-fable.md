# Dispatch unification design — per-family migrate-vs-keep verdicts

**Design plan**: `plans/017-dispatch-unification-design-claude-fable.md`
**Analyzed**: 2026-07-12, branch `docs/dispatch-unification-design`, base commit `69ec900`
(`docs(plans): mark 012 DONE`) on the `improve/integration` line — **not yet on `main`**.
**Assumption this design is built on**: plan 015 (notification fan-out dedup) is treated as
landed baseline, because it is — merged to `improve/integration` @ `947bc92` per
`plans/README-claude-fable.md`'s row 015, verified live in `plug-core/src/notifications.rs`
at the commit analyzed here (see §0). Plan 016 (daemon.rs module split) is **not** landed:
`plug/src/daemon.rs` is still a single 5,361-line file; `plug/src/daemon/mcp_dispatch.rs`
does not exist. Every `plug/src/daemon.rs` citation below is a line number into that single
file, not a future submodule path.

**Truth-rules disclaimer** (per `docs/TRUTH-RULES.md` / repo `CLAUDE.md`): this document
describes **intended work**. Nothing in §4's migration sequence is done until it lands on
`main` as its own PR with its own parity-test additions. `main` is the only source of "done
now" — check `docs/PROJECT-STATE-SNAPSHOT.md` for what has actually shipped. This design
itself is off-main (branch `docs/dispatch-unification-design`) and must not be cited as
current state by any future reader.

**Scope note**: this document is read-only research output. No code changed to produce it —
`git status` at the end of this branch's work shows only this new file.

---

## §0. What's already shared vs. what's duplicated (grounding correction)

Before the matrix: `plug-core/src/dispatch/mod.rs` (109 lines total) is *not* a partial
migration of business logic — it is a thin adapter shell for exactly one method family
(`tools/call`, `dispatch_tools_call` at `plug-core/src/dispatch/mod.rs:77-109`). Its own
module doc says so explicitly (`mod.rs:1-16`): "the routing core... is already
transport-agnostic and shared... Only `tools/call` is migrated here today."

Every other method family's *business logic* already lives in one shared `ToolRouter`
method, called identically by all three transport shells:

| Family | Shared implementation | Per-transport callers |
|---|---|---|
| `resources/read`, `prompts/get`, `completion/complete` | `plug-core/src/proxy/completion.rs:4,37,78` | `handler.rs:640,657,693`; `server.rs:1461,1488,1557`; `daemon.rs:2287,2312,2334` |
| `resources/subscribe`, `resources/unsubscribe` | `plug-core/src/proxy/subscriptions.rs:823,866` | `handler.rs:671,684`; `server.rs:1513,1538`; `daemon.rs:2551,2581` |
| `tasks/list`, `tasks/get`, `tasks/result`, `tasks/cancel` | `plug-core/src/proxy/tasks.rs:126,134,187,277` | `handler.rs:515,555,569,581`; `server.rs:1353,1376,1399,1422`; `daemon.rs:2453,2477,2501,2525` |
| `tools/list`, `resources/list`, `resources/templates/list`, `prompts/list` | `plug-core/src/proxy/catalog.rs:384,605,623,643` | `handler.rs:502,624,632,648`; `server.rs:1303,1441,1451,1478`; `daemon.rs:2259,2266,2273,2294` |
| shared error vocabulary | `plug-core/src/error.rs:5-49` (`ProtocolError`, one JSON-RPC code per case) | every site above via `McpError::from(ProtocolError::...)` |

So the "3× tax" the plan's "Why this matters" section warns against presupposing is, for
every family except `tools/call`, **already down to one shared function** — what's
triplicated is the ~10-40 line *adapter shell* around each call (param extraction, identity
construction, wire encoding), not the logic. This matters for every verdict below: a
"migrate" call here is a claim about shell consolidation and parity-surface hygiene, not
about deduplicating business logic that's already deduplicated.

---

## §1. Method inventory matrix

Built by reading the three dispatch surfaces end to end at `69ec900`:
`plug-core/src/proxy/handler.rs` (696 lines, `ServerHandler for ProxyHandler`, stdio),
`plug-core/src/http/server.rs` fn `handle_request` (lines 1240-1599, HTTP), and
`plug/src/daemon.rs` fn `dispatch_mcp_request` (lines 2231-2592, IPC).

Stdio's surface is the union of what `ProxyHandler` overrides *and* the rmcp-1.7.0 crate's
own `ServerHandler` default methods (`server_handler_methods!` macro,
`~/.cargo/registry/.../rmcp-1.7.0/src/handler/server.rs:163-371`, pinned dependency — not
plug code). Every method rmcp's `ClientRequest` enum knows about is in fact overridden by
`ProxyHandler` today, so the rmcp defaults below are dead code on this path but define what
"unmatched method" means for stdio specifically.

| Method | Stdio | HTTP | IPC | In `dispatch/`? | Behavior notes |
|---|---|---|---|---|---|
| `initialize` | `handler.rs:206-404` (`ServerHandler::initialize`) | `server.rs:1249-1281` (`ClientRequest::InitializeRequest`) | **no MCP-method analog** — IPC has its own pre-dispatch handshake, `IpcRequest::Register` at `daemon.rs:1614-1712`, not routed through `dispatch_mcp_request` | no | Three different shapes on purpose: stdio/HTTP are real MCP `initialize`; IPC's `Register` is an IPC-protocol-version handshake plus session creation, richer (returns `client_id`+`session_id`, enforces `MAX_REGISTERED_PROXY_CLIENTS`) than either MCP `initialize` response. Not a method-family candidate for `dispatch/` — it's transport bootstrap, not MCP method routing. |
| `ping` | rmcp default, always `Ok(())` (`server.rs` (rmcp) `:175-180`) — `ProxyHandler` does not override it | `server.rs:1283-1290` (`ClientRequest::PingRequest`) — validates session then returns `EmptyResult` | **no case in `dispatch_mcp_request`'s match** — falls to the `_ =>` arm at `daemon.rs:2587-2590`, `UNSUPPORTED_METHOD`. `IpcRequest::Ping{session_id}` (`daemon.rs:1803-1817`) is a *different*, IPC-protocol-level liveness check, not the MCP `ping` method | no | **Real gap, not just an encoding difference**: an MCP client that sends JSON-RPC `ping` over the IPC proxy today gets `UNSUPPORTED_METHOD`, where stdio/HTTP both succeed. Zero parity-test coverage catches this (§3). |
| `tools/list` | `handler.rs:489-506` | `server.rs:1292-1311` | `daemon.rs:2240-2260` | no | Shared impl (`catalog.rs:384`); per-shell differences are lazy-session-key construction (`DownstreamTransport::{Stdio,Http,Ipc}`) and client-type lookup mechanics only. Parity-tested (`daemon.rs:5188-5205`). |
| `tools/call` | `handler.rs:518-544` | `server.rs:1313-1345` | `daemon.rs:2370-2441` | **yes** (`dispatch/mod.rs:77`) | The one migrated family. Parity-tested including task-augmentation divergence (`daemon.rs:5096-5165`, stdio rejects, HTTP/IPC create a task — intentional, pinned). |
| `tasks/list` | `handler.rs:508-516` | `server.rs:1347-1368` | `daemon.rs:2443-2454` | no | Shared impl (`tasks.rs:126`). IPC resolves `TaskOwner` itself pre-call (`daemon.rs:2446-2452`, `UNKNOWN_SESSION` short-circuit) rather than via the `DownstreamContext::task_owner()` the `tools/call` dispatcher already has. **Not parity-tested** — absent from `assert_parity`'s method list (`daemon.rs:4972-4992`). |
| `tasks/get` | `handler.rs:546-558` | `server.rs:1370-1391` | `daemon.rs:2456-2478` | no | Same shape as `tasks/list`. Not parity-tested. |
| `tasks/result` | `handler.rs:560-572` | `server.rs:1393-1414` | `daemon.rs:2480-2502` | no | Same shape. Not parity-tested. |
| `tasks/cancel` | `handler.rs:574-582` | `server.rs:1416-1437` | `daemon.rs:2504-2526` | no | Same shape. Not parity-tested. |
| `resources/list` | `handler.rs:619-625` | `server.rs:1439-1447` | `daemon.rs:2262-2267` | no | Shared impl (`catalog.rs:605`), infallible (no `Result`). Parity-tested (`daemon.rs:5206-5219`). |
| `resources/templates/list` | `handler.rs:627-633` | `server.rs:1449-1457` | `daemon.rs:2269-2274` | no | Shared impl (`catalog.rs:623`), infallible. Parity-tested (`daemon.rs:5220-5233`). |
| `resources/read` | `handler.rs:635-641` | `server.rs:1459-1474` | `daemon.rs:2276-2288` | no | Shared impl (`completion.rs:4`). IPC pre-validates `uri` presence itself (`daemon.rs:2277-2285`, `INVALID_PARAMS`) before the shared call; stdio/HTTP rely on serde deserialization of `ReadResourceRequestParams` failing upstream of the handler if `uri` is absent. Parity-tested incl. unknown-uri (`daemon.rs:5234-5262`). |
| `prompts/list` | `handler.rs:643-649` | `server.rs:1476-1482` | `daemon.rs:2290-2295` | no | Shared impl (`catalog.rs:643`), infallible. Parity-tested (`daemon.rs:5263-5276`). |
| `prompts/get` | `handler.rs:651-661` | `server.rs:1484-1503` | `daemon.rs:2297-2313` | no | Shared impl (`completion.rs:37`). IPC pre-validates non-empty `name` itself (`daemon.rs:2298-2306`, `INVALID_PARAMS`). Parity-tested incl. unknown-prompt (`daemon.rs:5277-5304`). |
| `completion/complete` | `handler.rs:689-695` | `server.rs:1555-1570` | `daemon.rs:2315-2335` | no | Shared impl (`completion.rs:78`), takes no downstream-identity parameter at all. Parity-tested (`daemon.rs:5305-5328`). |
| `resources/subscribe` | `handler.rs:663-672` | `server.rs:1505-1528` | `daemon.rs:2528-2555` | no | Shared impl (`subscriptions.rs:823`). All three shells hand-construct `NotificationTarget::{Stdio,Http,Ipc}` inline rather than reuse `DownstreamCallContext::notification_target()` (`proxy/mod.rs:272-284`), which already derives the identical value from the same `DownstreamContext` the `tools/call` dispatcher uses. Parity-tested (`daemon.rs:5349-5355`). |
| `resources/unsubscribe` | `handler.rs:674-687` | `server.rs:1530-1553` | `daemon.rs:2557-2585` | no | Same shape as subscribe. Parity-tested (`daemon.rs:5356-5361`). |
| `logging/setLevel` | `handler.rs:600-617` | `server.rs:1572-1589` | `daemon.rs:2337-2368` | no | Shared mutation (`set_client_log_level` + `forward_set_level_to_upstreams`, called identically by all three), but **not** a `ToolRouter` method the shells call once — each shell duplicates the `tracing::info!` + two-call sequence verbatim. **Not parity-tested.** |
| `notifications/cancelled` (client→server notification, not a request) | `handler.rs:584-598` (`on_cancelled`) | `server.rs:596-607` (`ClientNotification::CancelledNotification` arm in `post_mcp`) | no case — IPC has no notification channel from client to daemon for this; cancellation over IPC flows through `forward_cancel_from_downstream` only when reachable via the stdio/HTTP notification path | no | Out of scope per §5 (notifications are plan 015's territory) — listed here only because it appears in the stdio/HTTP match statements the plan's step 1 says to read end-to-end. |

**Verify note for the plan's step 1 instruction** ("every method named in any of the three
match-chains appears as a row"): the HTTP `_ =>` catch-all (`server.rs:1591-1597`) and the
IPC `_ =>` catch-all (`daemon.rs:2587-2590`) both confirm no other `ClientRequest`/method-name
variant is handled beyond the rows above — cross-checked against rmcp's `ClientRequest` enum
arms enumerated in `rmcp-1.7.0/src/handler/server.rs:25-133` (`initialize`, `ping`,
`complete`, `set_level`, `get_prompt`, `list_prompts`, `list_resources`,
`list_resource_templates`, `read_resource`, `subscribe`, `unsubscribe`, `call_tool`,
`list_tools`, `custom_request`, `list_tasks`, `get_task_info`, `get_task_result`,
`cancel_task`) — every one of those has a row above except `CustomRequest`, which
`ProxyHandler` does not override (rmcp default: `on_custom_request`, not implemented, not
reachable via any of plug's three transports today) and is therefore out of scope as
not-currently-exposed surface, not a migration candidate.

---

## §2. `DownstreamContext` extension analysis

Read at `plug-core/src/dispatch/mod.rs:46-68`. Today's trait:

```rust
pub trait DownstreamContext: Send + Sync {
    fn downstream_call_context(&self) -> DownstreamCallContext;
    fn supports_tasks(&self) -> bool { true }
    fn task_owner(&self) -> Result<TaskOwner, McpError>;
}
```

`DownstreamCallContext` itself (`plug-core/src/proxy/mod.rs:197-284`) already carries:
`transport: DownstreamTransport` (`Stdio | Http | Ipc`, `proxy/mod.rs:188-195`), `client_id`,
`request_id`, `client_type`, `trace_id`, plus a derived `notification_target()` method
(`proxy/mod.rs:272-284`) that maps `transport` + `client_id` onto the matching
`NotificationTarget::{Stdio,Http,Ipc}` variant.

**Finding, walked field-by-field against every method family in §1's "migrate" candidates
(tasks/*, resources/subscribe+unsubscribe, the four list families, logging/setLevel):**

| What a method needs | Already on the trait/context? | Evidence |
|---|---|---|
| Client identity (for lazy-session-key / log-level keying) | Yes — `downstream_call_context().client_id` + `.transport` | `ToolRouter::lazy_session_key(DownstreamTransport, &str)` already takes exactly these two fields; every `tools/list`-family shell already builds it this way (`handler.rs:500-501`, `server.rs:1299-1302`, `daemon.rs:2250-2253`) |
| Client type (for list filtering) | Yes — `downstream_call_context().client_type` | Same three call sites read `client_type` from their own transport-specific store today; nothing new needed, just routed through the trait instead of read ad hoc |
| Notification/subscription target | Yes — `downstream_call_context().notification_target()` | `proxy/mod.rs:272-284`; **currently unused by the subscribe/unsubscribe shells**, which hand-roll the identical `NotificationTarget` construction inline (`handler.rs:668-669`, `server.rs:1508-1510`, `daemon.rs:2545-2546`) — a pre-existing small duplication independent of any dispatch migration |
| Task owner | Yes — `task_owner()`, already in the trait | IPC's task-family shells (`daemon.rs:2446-2452` etc.) already resolve ownership the same way `IpcDownstreamContext::task_owner()` does (`daemon.rs:735-748`), just inline instead of through the trait |
| Session-vanished-mid-call short-circuit (IPC's `UNKNOWN_SESSION`) | Not a trait concern — a call-site pattern | `tools/call`'s IPC shell already established the pattern: resolve `TaskOwner` *before* constructing `IpcDownstreamContext` so the transport-specific error frame (not a generic `McpError`) is preserved (`daemon.rs:2401-2416`, comment explains why). Any tasks/* migration reuses this pattern verbatim — no trait change |
| Reverse-request bridge (elicitation/sampling), progress/cancellation registration | Out of scope (§5) | The trait's own doc (`mod.rs:43-45`) already says it does not abstract the bridge mechanism; it flows through `DownstreamCallContext` + the existing `register_downstream_bridge`/`DownstreamBridge` (`proxy/mod.rs:340`) machinery unchanged |
| Client capabilities (`ClientCapabilities`) | Not needed by any family in §4's migrate list | Every family that would need it (elicitation/sampling gating) is out of scope; `completion/complete`, `resources/read`, `prompts/get` etc. take no downstream-identity parameter at all in their shared `ToolRouter` implementation (`completion.rs:4,37,78`) |

**Conclusion: the trait needs zero new fields or methods to serve every family in §4's
migrate list.** The existing three methods (`downstream_call_context`, `supports_tasks`,
`task_owner`) plus the already-derived `notification_target()` helper cover every
per-transport input those families need. This is the single biggest de-risking fact in this
design: "extend `DownstreamContext`" is not blocking work for any proposed migration step —
each step is purely "replace this shell's body with a call into a new shared
`dispatch::dispatch_X` function," exactly mirroring how `dispatch_tools_call` replaced the
three `tools/call` bodies. `supports_tasks()` stays `tools/call`-specific (only that family
branches sync-vs-task) and does not need to generalize.

---

## §3. Error-encoding matrix

Rows built from the error-construction sites in each transport, cross-checked against the
shared `ProtocolError` vocabulary (`plug-core/src/error.rs:5-49`) that every `ToolRouter`
method already funnels through.

| Failure class | Stdio | HTTP | IPC | Keep/normalize |
|---|---|---|---|---|
| **Method not found** (unmatched `ClientRequest` variant) | rmcp's own `Service<RoleServer>` blanket impl (`rmcp-1.7.0/src/handler/server.rs:19-161`, `handle_request` specifically at `:20-134`) dispatches by enum variant; every variant plug's `ClientRequest` enum has is handled by `ProxyHandler`, so this path is currently dead for stdio | `server.rs:1591-1597`, explicit `_ =>` arm: `ErrorData::new(ErrorCode::METHOD_NOT_FOUND, "method not supported", None)` — numeric JSON-RPC `-32601`, wrapped in a normal `ServerJsonRpcMessage::error` envelope | `daemon.rs:2587-2590`, explicit `_ =>` arm: `IpcResponse::Error{code: "UNSUPPORTED_METHOD", message: ...}` — a **string** code in IPC's own control-plane vocabulary, not a JSON-RPC numeric code at all | **Keep, documented**: IPC's error envelope is a different wire format by design (`IpcResponse::Error{code:String,...}` vs. JSON-RPC's numeric `ErrorCode`) — that's a transport-envelope decision from before this plan, not something a shared dispatcher should silently change. A migration should not attempt to make IPC emit numeric JSON-RPC codes; `ipc_from_mcp_result`/`ipc_ok` (`daemon.rs:2204-2228`) already bridge the shared `McpError` (numeric-code) case into IPC's envelope correctly for every family that reaches the shared router — the only asymmetry is the small set of param-shape/pre-router short-circuits (below). |
| **Upstream error passthrough** (tool/resource/prompt call fails upstream) | via `McpError` from `ToolRouter::call_tool_inner` / `completion.rs` methods, which map `rmcp::service::ServiceError::McpError(e) => e` and anything else to `McpError::internal_error` (`completion.rs:31-34,70-73`) | identical — same shared code, HTTP just wraps the resulting `McpError` in `ServerJsonRpcMessage::error` | identical — same shared code, wrapped via `ipc_from_mcp_result` | **Already unified.** This is the strongest evidence for §0's framing: nothing to normalize here, it's one code path already. |
| **Upstream timeout / unavailable** | `ProtocolError::Timeout`/`ServerBusy`/`ServerUnavailable` → JSON-RPC `-32603` (`error.rs:26-31`), constructed once in `ToolRouter::call_tool_inner` (`ServerUnavailable`/`ServerBusy` at `proxy/mod.rs:1656-1712`, `Timeout` at `proxy/mod.rs:1965`) | same | same | **Already unified** — same reasoning as above; the numeric code (`-32603`, "Internal error") is arguably too coarse (three distinct failure modes collapse to one code), but that's a pre-existing shared-router decision, not a per-transport divergence this design should touch. |
| **Invalid params (well-formed JSON-RPC envelope, malformed method params)** | Deserialization of `ClientRequest`'s tagged-enum variant happens inside the pinned `rmcp` transport/model code before `ProxyHandler` ever runs — not plug code, not inspected further here (out of the repo) | **Two tiers, and they disagree**: (1) if the whole POST body fails to parse as `ClientJsonRpcMessage` at all, `post_mcp` returns `HttpError::BadRequest("invalid JSON-RPC message")` (`server.rs:573-576`) — **plain-text HTTP 400, not a JSON-RPC error envelope, no request id preserved**, because parsing failed before an id could be extracted; (2) once inside a matched arm, HTTP never constructs its own invalid-params error — it relies entirely on tier-1 parsing, since `ClientRequest` is one big tagged enum and a param-shape mismatch fails deserialization at tier 1, not inside `handle_request` | Explicit per-arm `INVALID_PARAMS` string-coded `IpcResponse::Error` after `serde_json::from_value` fails, *inside* `dispatch_mcp_request` (e.g. `daemon.rs:2277-2285` for `resources/read`'s missing `uri`, `daemon.rs:2371-2387` for `tools/call`) — preserves session context (session was already validated before this point) and gives a method-specific message | **Normalize candidate, needs an operator decision, not a silent fix**: HTTP's tier-1 400-with-no-envelope response is the most different of the three (not JSON-RPC shaped at all) and is arguably the right one to flag for the parity matrix even without a shared dispatcher, since it's a pre-existing asymmetry not touched by any prior plan. Any dispatch migration that adds shared "invalid params" handling should NOT try to normalize stdio's rmcp-owned behavior (out of plug's control) but *could* give HTTP a JSON-RPC-shaped invalid-params response once inside `handle_request`'s match — that only helps arms that do their own extra validation, which today is none, since HTTP relies on the enum-level parse. Recommend: leave as a documented, individually-approved follow-up, not bundled into this migration. |
| **Auth required** | N/A — stdio is a loopback-trust, single-client-per-process transport with no bearer-token concept | `HttpError::Unauthorized` / `HttpError::UnauthorizedWithMetadata` from `validate_bearer_auth` middleware (`server.rs:445-504`), applied *before* `handle_request` runs at all — HTTP 401 + `WWW-Authenticate` header, non-JSON-RPC body (`http/error.rs:55-93`) | N/A for the MCP method surface — IPC's `AUTH_REQUIRED`/`AUTH_FAILED` codes (`daemon.rs:1136-1155`) gate **admin commands only** (`RestartServer`, `Reload`, `Shutdown`, `InjectToken` — see `ipc::requires_auth`), never `IpcRequest::McpRequest`. The Unix socket itself is the trust boundary, same model as stdio | **Keep — not a dispatch concern.** This is a real, structural asymmetry (only HTTP has downstream bearer auth) that pre-dates and is orthogonal to method dispatch; a shared `dispatch::dispatch_X` function would sit *after* this gate on every transport, so it cannot see or change this row. Listed for completeness per the plan's required section, verdict is "no action." |

---

## §4. Per-family verdict and migration order

Every verdict below is **migrate** (thin shell replacement into `dispatch/`, mirroring
`dispatch_tools_call`) or **keep** (leave the per-transport shell as-is), with the evidence
from §1-§3 that grounds it. Per the plan: "keep everything, document the pattern" is an
allowed outcome and is *not* what this section concludes — several families have concrete,
evidenced reasons to migrate (closing the ping gap, adding tasks/* and logging/setLevel to
the parity gate), so the overall verdict is **partial migration**, argued family by family
below rather than assumed.

**`tools/call` itself: verdict = migrate, already done.** It is the one family already living
in `plug-core/src/dispatch/mod.rs:77-109` (PR #63, `docs/PLAN.md:100-109`). Listed here only
for completeness against §1's matrix — no further action, not part of §4's sequence below.

### Migrate

| Family | Shell size (median across 3 transports) | Why migrate | Parity coverage today |
|---|---|---|---|
| `tools/list`, `resources/list`, `resources/templates/list`, `prompts/list` | ~10-20 lines each | Cheapest possible migration: infallible, zero error-encoding divergence (§3 n/a), §2 proved zero trait growth needed. Consolidating four call sites × three transports = 12 shells down to 4 shared functions removes real (if small) duplication for no risk. | Already parity-tested (`daemon.rs:5188-5233,5263-5276`) — a migration here is a pure refactor under an existing safety net, the safest possible starting point |
| `completion/complete` | ~10-15 lines | Needs literally no downstream context (§2) — the shell is param extraction + one call + wire encoding, nothing else. Lowest-risk non-list family. | Parity-tested (`daemon.rs:5305-5328`) |
| `resources/read`, `prompts/get` | ~15-20 lines | Shared impl already; only wrinkle is IPC's early param-presence check (§1), which a shared `dispatch_resources_read`/`dispatch_prompts_get` can absorb as a pre-check inside the shared function itself (return the same `INVALID_PARAMS`-shaped outcome IPC's shell returns today, encoded by each transport per its own convention — see §3's keep verdict on that row) | Parity-tested incl. not-found rows (`daemon.rs:5234-5262,5277-5304`) |
| `resources/subscribe`, `resources/unsubscribe` | ~15-25 lines | Shared impl already; migration also fixes the pre-existing `NotificationTarget` duplication identified in §2 (use `downstream_call_context().notification_target()` instead of hand-rolling per shell) — a genuine simplification, not just code motion | Parity-tested (`daemon.rs:5349-5361`) |
| `logging/setLevel` | ~10-15 lines | Not just shell duplication — the `tracing::info!` + `set_client_log_level` + `forward_set_level_to_upstreams` three-call sequence is copy-pasted verbatim three times (`handler.rs:600-617`, `server.rs:1572-1589`, `daemon.rs:2337-2368`) with only the identity source differing. **No parity test exists** — migrating this family is also the cheapest way to add parity coverage for it, since a single shared function makes "assert stdio/HTTP/IPC agree" trivial to write once | **Gap**: not in `assert_parity`'s method list (`daemon.rs:4972-4992`) — a migration PR should add the parity row as part of the same change, per the plan's step-4 requirement that migration PRs name their test coverage |
| `tasks/list`, `tasks/get`, `tasks/result`, `tasks/cancel` | ~15-20 lines each | Shared impl already (`tasks.rs`); §2 showed the `IpcDownstreamContext::task_owner()`-vs-manual-resolution pattern the family needs is already proven by `tools/call`'s IPC shell. Four families, same shape, natural to migrate together. | **Gap**: not in `assert_parity`'s method list — same "migrate PR adds the parity row" argument as `logging/setLevel` |
| `ping` | N/A for stdio/HTTP (already correct); IPC currently has **no MCP `ping` method at all** | This is the one family where "migrate" also means "fix a real behavior gap," not just deduplicate: today an MCP client sending `ping` through the daemon IPC proxy gets `UNSUPPORTED_METHOD` where stdio and HTTP both succeed (§1). A trivial `dispatch_ping` (no downstream context needed — even less than `completion/complete`) that IPC's shell can finally implement closes this gap cheaply. | **Gap, and worse than the others**: not only untested, the behavior itself is wrong on one transport today. Recommend this be its own tiny PR (bug-fix flavored, even though it rides on the same `dispatch/` pattern) rather than bundled with the rest of the list-family batch, so the fix is easy to point to independently in review. |

### Keep (adapter shell stays per-transport)

| Family | Why keep |
|---|---|
| `initialize` / IPC `Register` | Not a method-dispatch family — transport bootstrap with genuinely different session/handshake semantics per transport (§1). A shared `dispatch_initialize` would have to abstract session creation itself, which is a different (and much larger) problem than "adapt one already-shared router call." Out of this design's frame entirely. |
| `notifications/cancelled` and all other client→server notifications | Explicitly out of scope per this plan's §5 and per plan 015's already-landed fan-out unification, which owns the notification surface. Listed in §1 only because it appears in the match statements the plan's step 1 requires reading end-to-end. |
| Reverse requests (elicitation/sampling forwarding), progress/cancellation registration | Out of scope per the plan (§5 below); the trait's own doc already disclaims abstracting the bridge mechanism (`mod.rs:43-45`). |
| Auth-gating middleware (HTTP bearer auth, IPC admin-command auth) | Sits structurally *above* method dispatch (§3) — not a per-method concern a `dispatch::dispatch_X` function could see or change even if migrated. |

### Migration order (cheapest-first, revised from the plan's candidate order against the
matrix above — same overall shape, reordered where evidence changed the ranking)

1. **List family** (`tools/list`, `resources/list`, `resources/templates/list`,
   `prompts/list`) — 4 shared functions replacing 12 shells. Files touched:
   `plug-core/src/dispatch/mod.rs` (new `dispatch_list_*` fns), `plug-core/src/proxy/handler.rs`,
   `plug-core/src/http/server.rs`, `plug/src/daemon.rs`. Existing coverage:
   `daemon.rs:5188-5233,5263-5276` (no new test needed, existing parity rows keep passing as a
   characterization guard, same pattern PR #63 used per `docs/PLAN.md:104-107`).
2. **`completion/complete`** — smallest non-list family, zero context needed. Same files.
   Coverage: `daemon.rs:5305-5328`.
3. **`resources/read`, `prompts/get`** — absorb IPC's param-presence pre-check into the shared
   function. Same files. Coverage: `daemon.rs:5234-5262,5277-5304`.
4. **`resources/subscribe`/`unsubscribe`** — also lands the `notification_target()` cleanup
   from §2. Same files plus `plug-core/src/proxy/mod.rs` untouched (helper already exists).
   Coverage: `daemon.rs:5349-5361`.
5. **`logging/setLevel`** — needs a *new* parity-test row (none exists) added in the same PR;
   model it on `assert_parity`'s existing shape (`daemon.rs:5026-5036`).
6. **`tasks/*`** (list/get/result/cancel, four sub-steps or one combined PR — they share one
   shape) — needs four new parity rows in the same PR, same pattern.
7. **`ping`** — smallest change, but sequence it *last* specifically because it's the one
   family with an actual behavior change (a bug fix, not a pure refactor), so it should be
   reviewable in isolation from the larger mechanical migration and land with its own
   before/after note plus a new parity row proving IPC now agrees with stdio/HTTP.

Each step is independently shippable and revertible; none depends on another landing first
(unlike, e.g., plan 015→016's sequencing, which is a real ordering constraint — this list is
not). A migration PR that does NOT add or update a parity-test row per its own family (steps
5-7) has not met this design's evidence bar and should not merge as "done."

---

## §5. Explicit non-goals

- **No wire-behavior changes** except the one flagged, individually-approvable candidate in
  §3 (HTTP's tier-1 parse-failure response shape) — and even that is listed as a documented
  follow-up requiring its own operator decision, not bundled into any step in §4's sequence.
- **No transport feature loss.** Every family in §4's migrate list keeps its current
  behavior; `ping`'s IPC fix is additive (closes a gap), not a behavior change to stdio/HTTP.
- **No changes to `DownstreamTransport`/`NotificationTarget` variants.** Both are final per
  the KTD3 identity split (`DownstreamTransport::Ipc` at `proxy/mod.rs:194`,
  `NotificationTarget::Ipc` at `notifications.rs:54`) — §2 shows they don't need to change to
  serve any family in this design.
- **Reverse requests (elicitation/sampling) are out of scope.** Untouched by every step in
  §4; the bridge mechanism (`DownstreamBridge`, `proxy/mod.rs:340`) stays exactly as-is.
- **Notifications (plan 015's fan-out territory) are out of scope**, including the two
  documented-not-fixed behavior drifts from that plan's review (daemon pushes
  `AuthStateChanged` as a native unfiltered `IpcResponse` at `daemon.rs:1442-1451`, where
  stdio/HTTP flatten it into a broadcast logging message at `handler.rs:319-330` /
  `server.rs:282-293`; and daemon no-ops on a closed control channel at `daemon.rs:1478`,
  `Err(RecvError::Closed) => {}`, where stdio and HTTP both `break` their fan-out loop on the
  same condition, `handler.rs:343` / `server.rs:309`). Both are real, verified at this commit,
  and both are inputs a future notification-focused plan should pick up — **not** this one.
  Flagged here only because the plan's drift notes named them as grounding context.
- **No change to the shared `ProtocolError`/`McpError` vocabulary** (`error.rs:5-49`). §3
  shows the coarse `-32603` collapsing of timeout/busy/unavailable is pre-existing and shared
  already; revisiting that granularity is a separate, error-taxonomy-focused design.
- **`initialize` / IPC `Register` and all client→server notifications stay out of `dispatch/`**
  per §4's keep list — they are not method-dispatch problems in the sense this design (or the
  plan) is scoped to.

## Maintenance notes

- This design doc is compound knowledge, not current truth (repo truth rules) — a future
  reader checks `docs/PROJECT-STATE-SNAPSHOT.md` and `docs/PLAN.md` for what has actually
  landed, not this file.
- Each migration PR born from §4 should carry its own parity-test additions (steps 5-7
  explicitly require *new* rows, not just "existing rows still pass") and, if this doc is
  still the reference at that time, update its checklist/coverage column to reflect the new
  test names.
- If plan 016 (daemon.rs module split) lands before any §4 step starts, every `plug/src/daemon.rs`
  line citation in this doc needs re-anchoring into whatever `plug/src/daemon/*.rs` submodule
  `dispatch_mcp_request` moves to — re-find by function name, not by the line numbers here.
