# Claim Registry

This registry inventories the major current-state and roadmap claim sources that must be verified
against code and tests.

| source | section | short claim | claimed state | feature area |
|---|---|---|---|---|
| `docs/PLAN.md` | Current State | major stabilization, protocol-surface, and protocol-correctness work is complete | current truth | project overview |
| `docs/PLAN.md` | Current State | notification forwarding (logging, tools/resource/prompt list_changed) is complete | current truth | notifications |
| `docs/PLAN.md` | Current State | completion forwarding across stdio, HTTP, IPC is complete | current truth | completions |
| `docs/PLAN.md` | Current State | subscription pruning and rebind on route refresh is resolved | current truth | subscription lifecycle |
| `docs/PLAN.md` | Remaining Work | roots, elicitation/sampling, legacy SSE, and OAuth remain open | current truth | Stream B |
| `docs/ROADMAP-AUDIT-2026-03-08.md` | Summary | `done: 15`, `partial: 4`, `missing: 4` | audited truth | roadmap status |
| `docs/ROADMAP-AUDIT-2026-03-08.md` | Checklist | each feature row classifies done / partial / missing with evidence | audited truth | all roadmap areas |
| `docs/plans/2026-03-07-feat-mcp-spec-compliance-roadmap-plan.md` | Current Status | merged-on-main table reflects shipped work | mixed: current truth + history | roadmap status |
| `docs/plans/2026-03-07-feat-mcp-spec-compliance-roadmap-plan.md` | Implemented by PR #31 | list_changed, downstream protocol validation, HTTP completion, todo 039 are implemented | current truth | Stream A follow-ups |
| `docs/plans/2026-03-07-feat-mcp-spec-compliance-roadmap-plan.md` | Not implemented in code yet | sampling, elicitation, roots, legacy SSE, OAuth remain missing | current truth | Stream B |
| `docs/plans/2026-03-07-feat-roadmap-tail-closeout-plan.md` | Overview / acceptance criteria | resource subscriptions and tail closeout are complete | historical plan with status | roadmap tail |
| `docs/plans/2026-03-08-feat-roots-forwarding-plan.md` | Overview | roots forwarding is the next smallest Stream B item | branch-scoped intended work | off-main feature plan |
| `docs/RISKS.md` | Tasks / truth drift sections | post-June-2026 features remain unimplemented and truth drift can recur | current truth | risks / governance |
| `CLAUDE.md` | Current Status | notification forwarding, cancellation/progress, resources/prompts, and live runtime reconfiguration are still incomplete | current truth claim, suspected stale | dev guidance |
| `todos/039-pending-p2-subscription-stale-after-route-refresh.md` | status / work log | stale subscriptions after route refresh are resolved | tracked issue state | subscription lifecycle |

## Notes

- `docs/PLAN.md`, `docs/ROADMAP-AUDIT-2026-03-08.md`, and the compliance roadmap plan are the
  main claim-bearing sources for current state.
- `CLAUDE.md` is also a claim source because it explicitly says it is a "current source of truth".
- many older phase plans contain historical implementation claims, but they are treated as
  historical unless they are still referenced as current truth elsewhere.
