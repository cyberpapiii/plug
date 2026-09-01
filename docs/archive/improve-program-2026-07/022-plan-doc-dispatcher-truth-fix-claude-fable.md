# Plan 022: Remove docs/PLAN.md's stale "dispatcher work remains" claims that contradict its own ✅ entries

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README-claude-fable.md` — unless a reviewer dispatched you and
> told you they maintain the index.
>
> **Drift check (run first)**:
> `git diff --stat e341625..HEAD -- docs/PLAN.md`
> If the file changed since this plan was written, re-locate the target
> section by its header (`## Designed-But-Deferred Program Phases`) and
> compare the "Current state" excerpts before proceeding; if the stale
> sentences are already gone, mark this plan DONE-by-drift in the index and
> stop.

## Status

- **Priority**: P2
- **Effort**: S
- **Risk**: LOW
- **Depends on**: none
- **Category**: docs
- **Planned at**: commit `e341625`, 2026-07-12

## Why this matters

`docs/PLAN.md` is a load-bearing truth document: the repo's CLAUDE.md truth
workflow tells every agent to read the snapshot first and PLAN.md second
before answering any "what is implemented?" question. Its
"Designed-But-Deferred Program Phases" section — a CURRENT-STATE status list
with ✅/🟡 markers — contains a bullet whose tail still claims two pieces of
work "remain" while the very next bullets in the same list mark both ✅ done.
This is not hypothetical damage: the stale sentence is the documented origin
of an agent-memory contamination that told a 2026-07-11 planning run the IPC
identity split was "deferred" when it had landed a month earlier via PR #66
(recorded in `plans/README-claude-fable.md`'s corrections log). Fixing the
sentence closes the contamination source; the sweep step checks the rest of
the file's current-state sections for the same failure shape.

## Current state

- `docs/PLAN.md` — the target section starts at the header
  `## Designed-But-Deferred Program Phases` (line 89 at the planned-at
  commit). It is a bullet list, one bullet per program phase, each opening
  with a bold phase name and a status marker.
- The stale bullet (line 94, "Transport `RequestDispatcher` + parity
  matrix") is marked 🟡 and ends with these two sentences:

  > The only remaining dispatcher item is the **`DownstreamTransport::Ipc`
  > identity split** (KTD3) — deferred to its own PR because
  > `NotificationTarget::Stdio` is the shared bridge/delivery key for both
  > the in-process stdio path and daemon IPC across ~64 sites; the parity
  > matrix now de-risks it. The full `ToolRouter` god-object decomposition
  > also remains a separate, larger refactor.

- Both claims are contradicted two lines later in the SAME list:
  - line 95: `**ToolRouter god-object decomposition** — ✅ done on `main`
    via PR #65: …`
  - line 96: `**`DownstreamTransport::Ipc` identity split (KTD3)** — ✅ done
    on `main` via PR #66: …`
  - line 97 (supervision bullet) even closes with: "**The
    operability/hardening program (items 1–4) is now fully landed.**"
- `docs/PROJECT-STATE-SNAPSHOT.md` agrees the program is complete (its line
  106: "the 2026-06-10 operability/hardening program is complete:
  degraded-vs-absent model (#61), transport dispatcher + whole-surface
  parity gate (#63/#64), ToolRouter god-object decomposition (#65), IPC
  identity split (#66), and supervision (#67)"). The snapshot needs NO
  edits: its own "still deferred: … KTD3" lines (117, 125) are bullets
  inside explicitly dated "On 2026-06-10, `main` absorbed PR #6x" changelog
  paragraphs — historical records that were true as of their date, which
  `docs/TRUTH-RULES.md` classifies as compound knowledge, not current truth.
- Repo truth conventions live in `docs/TRUTH-RULES.md` — read it before
  editing (labels: `done on main` / `partial on main` / `exists off-main` /
  `missing`; dated history stays, current-state text must match `main`).

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Locate section | `grep -n 'Designed-But-Deferred' docs/PLAN.md` | one match |
| Confirm removal | `grep -cn 'only remaining dispatcher item' docs/PLAN.md` | 0 matches |
| Scope check | `git status --short` | only `docs/PLAN.md` modified |

No cargo gates — this plan changes one markdown file.

## Scope

**In scope** (the only file you should modify):
- `docs/PLAN.md` — the `## Designed-But-Deferred Program Phases` section's
  dispatcher bullet, plus any additional stale current-state sentence the
  sweep (step 2) finds **in this same file**.

**Out of scope** (do NOT touch, even though they look related):
- `docs/PROJECT-STATE-SNAPSHOT.md` — verified consistent; its
  "still deferred" lines are dated history and must stay.
- Dated sections WITHIN `docs/PLAN.md` (headers like
  `## 2026-06-10 Transport Dispatcher — tools/call Slice …` and their
  bullets, e.g. the "Deferred to follow-up:" line under them) — historical
  record; leave verbatim.
- `docs/ROADMAP-AUDIT-2026-03-08.md`, `docs/hardening-log.md`, todos/ —
  historical/tracking docs; if the sweep surfaces staleness there, REPORT it
  in your completion notes instead of editing.

## Git workflow

- Branch: `advisor/022-plan-doc-dispatcher-truth-fix` off `main`.
- One commit, conventional style matching repo history (recent examples:
  `docs(snapshot): align off-main branch status`,
  `docs(installer): remove stale get.plug.sh usage`): suggested message:
  `docs(plan): mark dispatcher phase done; drop claims contradicted by its own entries`.
- Do NOT push or open a PR unless the operator instructed it.

## Steps

### Step 1: Fix the dispatcher bullet

In `docs/PLAN.md`'s `## Designed-But-Deferred Program Phases` section,
rewrite the dispatcher bullet (line 94):

1. Change its status marker: `🟡 mostly done on `main`` → `✅ done on `main``.
2. Keep the factual middle of the bullet (the PR #63/#64 description of what
   the parity gate covers and the `ipc_ok`/`ipc_from_mcp_result`
   consolidation) verbatim.
3. Replace the two stale closing sentences (quoted in "Current state") with
   a pointer to the adjacent entries, e.g.:

   > The two items this phase deferred — the `DownstreamTransport::Ipc`
   > identity split (KTD3) and the full `ToolRouter` decomposition — each
   > shipped as their own PR; see the ✅ entries below (PR #66, PR #65).

   Exact wording may vary; the requirements are: no "remaining"/"remains"
   phrasing describing landed work, and the KTD3 + decomposition references
   point at the ✅ bullets instead of claiming openness.

**Verify**:
`grep -n 'only remaining dispatcher item\|also remains a separate, larger refactor' docs/PLAN.md`
→ no matches.

### Step 2: Sweep the rest of PLAN.md's current-state sections

Run `grep -n 'remain\|remaining\|deferred\|next ' docs/PLAN.md`. For each
hit, classify:

- Inside a dated section (`## 2026-…` header or an "On 2026-…, `main`
  absorbed…" paragraph) → historical, leave it.
- Inside a current-state section (`## Remaining Work`,
  `## Designed-But-Deferred Program Phases`, or any undated section) →
  verify the claim against `docs/PROJECT-STATE-SNAPSHOT.md`'s Release
  Status section and, if the claim materially matters, against code on
  `main`. Fix only what is contradicted; leave true claims alone.

Known-true items you will hit and must NOT change (verified at the
planned-at commit): the `## Remaining Work` optional-scope list — live
runtime reconfiguration (genuinely out of scope), the ≥16MB artifact
`spawn_blocking` item (still open; plan 005 targets it), and the
metrics-recording e2e test item (still open).

**Verify**: for every hit you changed, note in the commit body which snapshot
line or code location contradicts the old text; for every hit you kept,
nothing to do.

### Step 3: Scope check and index update

**Verify**: `git status --short` → only `docs/PLAN.md` (and the plans README
status row edit); the CLAUDE.md post-merge truth-pass checklist items
"`docs/PLAN.md` still matches `main`" and "branch-only wording removed" are
now satisfiable.

## Test plan

Not applicable — docs-only. The verification greps in the steps are the
test.

## Done criteria

Machine-checkable. ALL must hold:

- [ ] `grep -c 'only remaining dispatcher item' docs/PLAN.md` → 0
- [ ] `grep -c 'also remains a separate, larger refactor' docs/PLAN.md` → 0
- [ ] The dispatcher bullet in `## Designed-But-Deferred Program Phases`
      starts with `✅` (`grep -A1 'Transport .RequestDispatcher' docs/PLAN.md`
      shows the ✅ marker)
- [ ] `docs/PROJECT-STATE-SNAPSHOT.md` is unmodified
      (`git diff --stat -- docs/PROJECT-STATE-SNAPSHOT.md` → empty)
- [ ] No files outside `docs/PLAN.md` + the plans README are modified
- [ ] `plans/README-claude-fable.md` status row updated

## STOP conditions

Stop and report back (do not improvise) if:

- The snapshot and PLAN.md genuinely DISAGREE about any phase's status
  beyond the known stale sentences (a truth conflict needs main-thread
  adjudication against code, not a drive-by doc edit).
- Step 2's sweep finds a current-state claim you cannot verify from the
  snapshot alone and whose truth would require reading substantial code —
  list it in the report instead of guessing.
- The stale sentences are already gone at execution time (someone fixed it)
  — mark DONE-by-drift, don't invent other edits to justify the branch.

## Maintenance notes

- The failure shape to watch in future reviews of PLAN.md edits: a phase
  lands and gets its own ✅ bullet or dated entry, but prose inside an
  EARLIER bullet still narrates it as future work. The post-merge truth-pass
  checklist in CLAUDE.md exists to catch exactly this; reviewers of
  roadmap-affecting PRs should grep PLAN.md's current-state sections for
  "remaining/deferred" naming the just-landed feature.
- Related contamination cleanup already done outside this plan: the plans
  README corrections log and the agent memory index were corrected on
  2026-07-11; this plan removes the root cause.
