# Main Truth Matrix

Baseline: `main` @ `f2cf637816c508e33042a5e6fe39a76afa98f4ef`

Legend:

- `done on main`
- `partial on main`
- `missing`

| feature | code evidence on `main` | test evidence on `main` | verdict | doc accuracy | next action |
|---|---|---|---|---|---|
| downstream HTTP bearer auth | `plug-core/src/auth.rs`, `plug-core/src/http/server.rs`, `plug/src/runtime.rs` | covered by HTTP auth tests and serve/runtime tests | done on main | accurately reflected in `docs/PLAN.md` and roadmap audit | none |
| downstream HTTPS | `plug-core/src/config/mod.rs`, `plug/src/runtime.rs` | HTTPS runtime test exists | done on main | accurate | none |
| logging forwarding | `plug-core/src/server/mod.rs`, `plug-core/src/proxy/mod.rs`, `plug-core/src/http/server.rs`, `plug/src/daemon.rs`, `plug/src/ipc_proxy.rs` | logging tests exist in proxy/server paths | done on main | accurate | none |
| tools/list_changed | stdio + HTTP fan-out exists; daemon IPC does not push it | HTTP SSE test exists | partial on main | accurate in audit doc; high-level docs should mention IPC gap | decide whether IPC parity is required |
| resources/list_changed | upstream handlers + coalesced refresh + stdio/HTTP fan-out exist; IPC masked false | covered by code, limited dedicated transport tests | done on main | accurate in roadmap audit and compliance plan | add targeted tests later |
| prompts/list_changed | upstream handlers + coalesced refresh + stdio/HTTP fan-out exist; IPC masked false | covered by code, limited dedicated transport tests | done on main | accurate in roadmap audit and compliance plan | add targeted tests later |
| progress routing | stdio + HTTP only; no IPC push frame support | stdio/HTTP tests exist | partial on main | accurate in audit doc; stale in `CLAUDE.md` because it still says incomplete generically | decide whether IPC parity is required |
| cancelled routing | stdio + HTTP only; no IPC push frame support | stdio/HTTP tests exist | partial on main | accurate in audit doc; stale in `CLAUDE.md` because it still says incomplete generically | decide whether IPC parity is required |
| resources/prompts/templates forwarding | proxy, HTTP, and IPC handlers exist on `main` | covered by integration tests and prior shipped paths | done on main | accurate in plan and audit docs; stale in `CLAUDE.md` | none |
| resource subscribe/unsubscribe lifecycle | subscription registry, rollback, disconnect cleanup, refresh-time prune/rebind exist on `main` | lifecycle/unit tests exist | done on main | accurate in plan and audit docs | none |
| completion forwarding | stdio, HTTP, and daemon IPC handlers exist on `main` | routing and serde tests exist; branch added HTTP handler | done on main | accurate in `docs/PLAN.md`; older docs may still understate HTTP | add direct HTTP completion test later if missing |
| structured output: `outputSchema` | preserved in proxy path | explicit test exists | done on main | accurate | none |
| structured output: `structuredContent` | passes through unmodified | no dedicated end-to-end proof found | partial on main | accurately marked partial in audit doc | add dedicated tests |
| structured output: `resource_link` | likely passes through unmodified | no dedicated end-to-end proof found | partial on main | accurately marked partial in audit doc | add dedicated tests |
| capability synthesis | generally honest on `main`; per-transport masking exists for IPC resource subscribe and list_changed | some synthesis tests exist | done on main | accurate in current docs | continue spot-checking transport-specific masking |
| meta-tool mode | fully implemented | direct tests exist across stdio/HTTP | done on main | accurate | none |
| daemon-backed local sharing | daemon runtime + shared routing exist | behavior exercised by existing tests | done on main | accurate | none |
| reconnecting IPC proxy sessions | reconnect logic exists | restart/reconnect tests exist | done on main | accurate | none |
| daemon continuity recovery | reconnect-based stdio-over-IPC recovery proven, not full cross-transport persistence | targeted tests only | partial on main | `docs/PLAN.md` is acceptable if read narrowly; broader wording would be misleading | keep wording narrow |
| session-store abstraction seam | trait + stateful impl exist | store tests exist | done on main | accurate | none |
| downstream MCP-Protocol-Version validation | incoming HTTP POST validation exists on `main` | validation code exists; test coverage should be strengthened | done on main | accurate in audit / roadmap docs | add explicit negative tests if absent |
| upstream MCP-Protocol-Version send-side | no explicit send-side implementation on `main` | none | missing | accurately tracked as smaller open item | implement when doing send-side follow-up |
| roots forwarding | not on `main` | none on `main` | missing | accurately listed as remaining work | branch candidate exists off-main |
| elicitation | not on `main` | none | missing | accurately listed as remaining work | off-main checkpoint candidate exists |
| sampling | not on `main` | none | missing | accurately listed as remaining work | off-main checkpoint candidate exists |
| legacy SSE upstream transport | not on `main` | none | missing | accurately listed as remaining work | off-main checkpoint candidate exists |
| OAuth upstream auth | not on `main` | none | missing | accurately listed as remaining work | off-main checkpoint candidate exists |

## Main Conclusions

- `docs/PLAN.md` and `docs/ROADMAP-AUDIT-2026-03-08.md` are broadly aligned to `main`.
- the largest stale current-state doc is `CLAUDE.md`
- the remaining `main` gaps are Stream B plus a few smaller follow-up items, not a hidden pile of unresolved Stream A work
