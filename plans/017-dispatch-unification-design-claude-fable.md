# Plan 017: Dispatch unification — per-family migrate-vs-keep verdicts and design across the three transports

> **Executor instructions**: This is a DESIGN plan — its deliverable is a
> design document, not code. Follow the steps; the investigation is
> read-only. If anything in the "STOP conditions" section occurs, stop and
> report. When done, update the status row in `plans/README-claude-fable.md`.
>
> **Drift check (run first)**: `git diff --stat e341625..HEAD -- plug-core/src/dispatch/ plug-core/src/proxy/handler.rs plug-core/src/http/server.rs plug/src/daemon.rs`
> Line anchors below will have drifted if plans 003/015/016 landed — re-find
> by name; the method inventory MUST be taken from live code, not this plan.
> Another AI agent (Codex) may be working in this repo concurrently.

## Status

- **Priority**: P3 (direction — prevents the half-migrated dispatcher from fossilizing)
- **Effort**: M (investigation + writing; no code)
- **Risk**: NONE (docs only)
- **Depends on**: none to write. Reads better AFTER 015 (fanout dedup) lands,
  since notifications leave the scope. The design should ASSUME 015 and 016
  are done (state that assumption inline).
- **Category**: direction / architecture
- **Planned at**: commit `e341625`, 2026-07-11

## Why this matters

`plug-core/src/dispatch/` was created to unify per-method request handling
across the three transports — its module doc (`mod.rs:15`) says **"Only
`tools/call` is migrated here today; other method families remain on their
per-transport paths until their own follow-up migrations."** Every OTHER MCP
method (tools/list, resources/*, prompts/*, completion, logging/setLevel,
subscribe/unsubscribe, ping, …) still has three per-transport handler
shells: stdio (`plug-core/src/proxy/handler.rs`), HTTP
(`plug-core/src/http/server.rs`), daemon IPC (`plug/src/daemon.rs`,
`dispatch_mcp_request`, ~360 lines).

Two facts temper the "3× tax" framing, and the design must weigh them
honestly rather than presuppose migration:

- `dispatch/mod.rs`'s own module doc states the routing core is ALREADY
  transport-agnostic and shared — the per-transport shells mostly adapt wire
  types and encode errors, so the duplicated surface is thinner than "three
  implementations" suggests.
- `docs/PLAN.md` records that the cross-transport parity matrix covers the
  entire method surface since PR #64 — drift between the shells is caught
  mechanically, not just by review. (Verify this claim against the live
  parity tests during step 1; treat it as a lead, not truth.)

The question this design answers is therefore NOT "how do we migrate
everything" but **"which method families (if any) are worth migrating, and
is `tools/call` an exemplar or a justified one-off?"** — "keep the adapter
shells, document the pattern" is an acceptable conclusion if the evidence
supports it. What is NOT acceptable is the current undocumented
half-state: new contributors can't tell which pattern to follow.

Groundwork that has ALREADY LANDED on `main` (correction 2026-07-11,
verified at `e341625` — earlier project notes describing this as deferred
are stale): the **IPC identity split** is done. `DownstreamTransport::Ipc`
exists (`plug-core/src/proxy/mod.rs:278`), `NotificationTarget` has a
first-class `Ipc` variant (`plug-core/src/notifications.rs:51`), and
`dispatch/mod.rs` already defines the `DownstreamContext` trait with
per-transport `supports_tasks`/`task_owner` hooks. The design therefore does
NOT need to sequence around an identity split — it builds directly on the
existing trait and per-transport identities.

## Deliverable

`docs/plans/2026-07-dispatch-unification-design-claude-fable.md` containing
the five sections below. Its primary output is a DECISION — a
migrate-vs-keep verdict per method family, grounded in the inventory — with
a migration design only for families where the verdict is "migrate". The
design doc must follow the repo's truth rules: it describes INTENDED work;
it must not claim anything is done.

### Required section 1 — Method inventory matrix

For every MCP method plug forwards or answers: rows = methods; columns =
{stdio handler location, HTTP handler location, IPC dispatch location,
already-in-dispatch/?, behavior differences noted}. Build it by reading the
three dispatch surfaces (the big match/if-chains) end to end. This matrix is
the design's evidence base and must be from live code.

### Required section 2 — DownstreamContext extension analysis

How the existing `DownstreamContext` trait (read
`plug-core/src/dispatch/mod.rs:46-68` — today it supplies
`downstream_call_context()`, `supports_tasks()`, and `task_owner()` for
tools/call) must grow to serve every method: what each transport can supply
for each field, which methods need client identity, capabilities, session
handles, subscription access, cancellation/progress registration. The three
per-transport identities (`DownstreamTransport::{Stdio,Http,Ipc}` and the
matching `NotificationTarget` variants) already exist on `main`, so this
section analyzes trait growth only — no identity work is needed.

### Required section 3 — Error-encoding matrix

How each transport encodes: method-not-found, upstream error passthrough,
upstream timeout/unavailable, invalid params, auth-required. Rows from live
code (find each transport's error construction sites). Differences here are
where unification silently changes wire behavior — each difference gets a
keep/normalize recommendation with a parity-test note.

### Required section 4 — Per-family verdict, then migration order

For each method family from the section-1 matrix: an explicit verdict —
**migrate** or **keep the adapter shell** — with the evidence (shell size,
behavior differences found, parity coverage, expected churn). "Keep
everything, document the adapter-shell pattern in `dispatch/mod.rs`'s doc"
is an allowed overall outcome and must be argued against, not assumed away.

For families with a "migrate" verdict only: a cheapest-first sequence of
PRs, each independently shippable and parity-tested. Candidate order to
validate against the matrix (adjust with reasons): read-only list methods
(tools/list, resources/list, prompts/list, templates) → completion →
logging/setLevel + subscribe/unsubscribe (needs plan 010's registry API) →
resources/read (artifact/chunking interplay) → ping/misc. For each step:
files touched, which existing tests cover it, which parity gaps need a new
test first.

### Required section 5 — Explicit non-goals

At minimum: no wire-behavior changes except those recommended in section 3
(each individually approved), no transport feature loss, no changes to the
existing transport identities (`DownstreamTransport`/`NotificationTarget`
variants are final), and reverse requests (sampling/elicitation) +
notifications (plan 015's fanout) are out of scope.

## Commands you will need

Read-only investigation:

| Purpose | Command |
|---------|---------|
| Dispatch state | `cat plug-core/src/dispatch/mod.rs` (and siblings: `ls plug-core/src/dispatch/`) |
| Method surfaces | read `plug-core/src/proxy/handler.rs`, `plug-core/src/http/server.rs`, daemon `dispatch_mcp_request` (post-016: `plug/src/daemon/mcp_dispatch.rs`) |
| Parity coverage | `grep -rn 'parity' plug-core/tests plug/tests -l` then read the matrix cases |

## Scope

**In scope**: the new design doc under `docs/plans/`; reading anything.

**Out of scope**: ALL code changes; changes to any existing doc (the design
references them; a one-line pointer added to `docs/PLAN.md` is allowed ONLY
if that file has a section listing active plan docs — check first, otherwise
skip).

## Git workflow

- Branch: `docs/dispatch-unification-design`
- Commit: `docs(plans): dispatch unification design (all-transport method migration)`.
- Do NOT push or open a PR unless the operator instructed it.

## Steps

1. **Inventory** — build section 1's matrix from the three surfaces. Verify: every method named in any of the three match-chains appears as a row.
2. **Context + error analysis** — sections 2 and 3 from live code. Verify: every section-3 row cites file:line for each transport's error construction.
3. **Verdicts + order + non-goals** — sections 4 and 5. Verify: every method family has a verdict with cited evidence; every proposed migration step names its parity coverage or the gap.
4. **Write and place the doc** — assemble at `docs/plans/2026-07-dispatch-unification-design-claude-fable.md` with a header stating: date, planned-at commit, the assumption that plans 015/016 land first, and the truth-rules disclaimer ("this describes intended work; nothing here is done until merged to main"). Verify: doc exists; `cargo test --workspace` untouched (no code changed — `git status` shows only the new doc).

## Test plan

Not applicable (docs only). Quality gate instead: a reader who has never seen
this session must be able to start migration step 1 from the doc alone —
check each migration step names concrete files, functions, and tests.

## Done criteria

- [ ] `docs/plans/2026-07-dispatch-unification-design-claude-fable.md` exists with all five sections
- [ ] Method matrix built from live code (spot-checkable file:line refs)
- [ ] Every method family carries an explicit migrate-vs-keep verdict with evidence ("keep the adapter shell" is an acceptable verdict)
- [ ] `git status` shows only the new doc (and optionally one pointer line in docs/PLAN.md)
- [ ] `plans/README-claude-fable.md` status row updated

## STOP conditions

- `plug-core/src/dispatch/` has migrated substantially beyond tools/call at
  execution time (someone else advanced it) — re-scope: the design becomes
  "finish the remainder", and the inventory shrinks; if MOST methods migrated,
  report that this plan may be moot.
- The three surfaces disagree so much that a shared dispatcher needs
  per-transport behavior flags for a majority of methods — report; the
  right design might then be "unify stdio+IPC only, leave HTTP" and that's an
  operator decision.

## Maintenance notes

- The design doc is compound knowledge, not current truth (repo truth rules) — future readers check `docs/PROJECT-STATE-SNAPSHOT.md` for what actually landed.
- Each migration PR born from section 4 should carry its own parity-test additions and update the design doc's checklist column.
