# Audit: MCP 2026-07-28 production readiness

**Date**: 2026-08-07  
**Branch**: `feat/mcp-2026-07-28-production-ready` (worktree)  
**Base HEAD**: `e767377`  
**Authoritative sources**: MCP 2026-07-28 spec, RMCP 3.1.0, `scripts/check-mcp-conformance.sh`, live client binary probe.

## Current architecture (verified)

Plug is a dual-face MCP gateway:

- Downstream server: stdio (`plug connect` / daemon IPC) and Streamable HTTP (`plug serve`)
- Upstream client: stdio, Streamable HTTP, legacy SSE with HTTP→SSE fallback
- SDK: `rmcp = "=3.1.0"`
- Default wire era: legacy `2025-11-25`
- Modern era: `2026-07-28`, gated by `http.modern_downstream_enabled` and `modern_upstream_enabled` (both default `false`)

PR #68 already landed dual-era adapters, discovery, sessionless modern HTTP, native modern↔modern MRTR, modern task extension advertisement (when gated on), and fail-closed mixed-era call suppression.

## Baseline (pre-edit on clean worktree)

| Gate | Result |
| --- | --- |
| `cargo test --workspace --lib` | 728 passed |
| `scripts/check-mcp-conformance.sh inventory` | ok |
| `scripts/check-mcp-conformance.sh local` | ok (proven-local suite) |

Unrelated dirty WIP on the primary checkout was isolated via worktree; not mixed into this branch.

## Requirements matrix (concise)

| Area | Classification | Evidence / note |
| --- | --- | --- |
| Lifecycle / discover / era select | already compliant (gated) | `protocol.rs`, HTTP modern path, stdio discover |
| UnsupportedProtocolVersion when gate off | already compliant | modern HTTP rejects when gate false |
| resultType modern / strip legacy | already compliant | `rewrite_legacy_result` |
| Sessionless modern HTTP / no session id | already compliant | http server modern arms |
| MRTR modern↔modern | already compliant | `native_modern_upstream_completes_a_real_two_round_tool_call` |
| Mixed-era MRTR | not applicable / dormant | fail-closed by design until bridge exists |
| Catalog cache `ttlMs`/`cacheScope` | required for modern; **implemented this run** | synthesized `0` + `private`; stripped on legacy |
| `subscriptions/listen` | optional valuable; not advertised | suppressed via `suppress_unimplemented_modern_capabilities` |
| Tasks extension | optional valuable; gated | advertised only when modern + supports_tasks |
| Apps/UI | not applicable | product does not embed MCP Apps UI |
| Roots / Sampling / Logging | legacy pass-through; no new modern dependency | still used for legacy clients |
| Remote OAuth | already compliant for HTTP | CIMD/DCR/PKCE/resource indicators |
| Stdio OAuth | not applicable | env/keychain credentials; no remote AS on stdio face |
| Hard cutover to modern-only | rejected | installed Cursor still `LATEST_PROTOCOL_VERSION=2025-11-25` (verified 2026-08-07) |

## Migration slices (this run)

1. Synthesize conservative catalog cache directives; strip on legacy rewrite.
2. Repair stale truth docs (`MCP-SPEC.md`, `CLAUDE.md`, `CRATE-STACK.md`, snapshot/PLAN notes).
3. Re-run local conformance + focused tests.
4. Attempt official modern conformance against a disposable local endpoint (operator-gated script).

## Official modern conformance (2026-08-07)

Ran `@modelcontextprotocol/conformance@0.2.0-alpha.10` with `--spec-version 2026-07-28`
against a disposable Plug HTTP endpoint (`modern_downstream_enabled=true`, empty
upstream set, isolated `HOME`). Results under `/tmp/mcp-official-full-IDI8RC`.

| Outcome | Scenarios |
| --- | --- |
| Passed | `tools-list`, `resources-list`, `prompts-list`, `server-sse-multiple-streams`, `dns-rebinding-protection` (6 SUCCESS checks) |
| Failed | 15 fixture-content scenarios (`tools-call-*`, `resources-read-*`, `prompts-get-*`, `completion-complete`) with `tool/prompt/resource not found: test_*` |

Interpretation: Plug's modern gateway lifecycle and empty-catalog list paths are
exercised by the official suite. The active suite also expects a fixed fixture
catalog (`test_simple_text`, `test://static-text`, etc.). An empty multiplexer
(or ordinary mock `echo` tools) cannot satisfy those rows. Closing that gap needs
either a suite-aligned fixture upstream or an `expected-failures` baseline, not
a silent claim of full certification.

## Remaining accepted exceptions

- Modern gates stay default-off until required production clients speak modern
  and fixture-backed official modern evidence is retained.
- `subscriptions/listen`, mixed-era multi-round, task+MRTR, Apps/UI remain suppressed.
- Official modern npm suite is still `0.2.0-alpha.10` (prerelease), not a stable certification.

## Risks / rollback

- Cache directive emission is additive on modern wire; legacy strip is the safety valve.
- Rollback: revert catalog/protocol commits; docs-only revert is independent.
- Never enable modern gates globally without real-client proof.
