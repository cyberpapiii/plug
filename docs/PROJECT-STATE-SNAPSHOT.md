# Project State Snapshot

Baseline: `main` @ `3e16fe4` (post-merge of PR #33 truth guardrails + PR #32 roots forwarding)

This is the canonical current-state doc for the project.

## What Is True On `main`

Implemented on `main`:

- downstream stdio via `plug connect`
- downstream Streamable HTTP via `plug serve`
- downstream HTTPS
- downstream bearer auth for non-loopback HTTP
- logging forwarding
- tools/resource/prompt list-changed forwarding for stdio + HTTP
- progress and cancelled routing for stdio + HTTP
- resources/prompts/templates forwarding
- resource subscribe/unsubscribe lifecycle
- completion forwarding across stdio, HTTP, and daemon IPC
- structured output pass-through, with strongest proof for `outputSchema`
- capability synthesis with per-transport masking
- meta-tool mode
- daemon-backed local sharing
- reconnecting IPC proxy sessions
- session-store seam / stateless prep
- downstream protocol-version validation
- roots forwarding with union cache across stdio, HTTP, and daemon IPC

Partial on `main`:

- daemon IPC notification parity beyond logging
- `structuredContent` and `resource_link` pass-through are present but under-proven by dedicated end-to-end tests
- daemon continuity recovery is proven narrowly for stdio-over-IPC reconnect, not as full cross-transport persistence

Missing on `main`:

- elicitation
- sampling
- legacy SSE upstream transport
- OAuth upstream auth
- upstream `MCP-Protocol-Version` send-side

## What Exists Off-Main

Candidate future state only:

- `fix/subscription-rebind-confidence` — large checkpoint branch containing extractable future work (OAuth, SSE client, elicitation/sampling scaffolds, research docs), not mergeable whole-cloth

Off-main work must not be described as current implementation.

## Documentation Taxonomy

Use docs by role:

- current truth:
  - `docs/PLAN.md`
  - `docs/ROADMAP-AUDIT-2026-03-08.md`
  - `docs/PROJECT-STATE-SNAPSHOT.md`
  - `docs/TRUTH-RULES.md`
- workflow enforcement:
  - `AGENTS.md`
  - `CLAUDE.md`
  - `docs/WORKFLOW-OPERATING-MODEL.md`
- issue tracking:
  - `todos/*.md`
- plans / intended work:
  - `docs/plans/*.md`
- historical / design context:
  - old phase plans and solutions docs

## Current Top Priorities

1. keep current-state docs aligned with `main`
2. clean up stale branches and worktrees from superseded development work
3. continue Stream B work (elicitation/sampling, legacy SSE, OAuth)
4. keep all off-main work clearly marked as candidate future state only
5. preserve the CE adapter layer (`AGENTS.md`, `CLAUDE.md`, workflow guide) so future agents start in the right place

## Audit Artifacts

- [BASELINE-2026-03-08](./audit/BASELINE-2026-03-08.md)
- [CLAIM-REGISTRY-2026-03-08](./audit/CLAIM-REGISTRY-2026-03-08.md)
- [MAIN-TRUTH-MATRIX-2026-03-08](./audit/MAIN-TRUTH-MATRIX-2026-03-08.md)
- [OFF-MAIN-STATE-2026-03-08](./audit/OFF-MAIN-STATE-2026-03-08.md)
- [DOC-RECONCILIATION-2026-03-08](./audit/DOC-RECONCILIATION-2026-03-08.md)

## Rule

If a statement conflicts with `main`, `main` wins.
