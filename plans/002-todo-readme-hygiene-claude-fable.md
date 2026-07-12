# Plan 002: Reconcile the todo tracker's contradictory statuses and fix README staleness

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README-claude-fable.md`.
>
> **Drift check (run first)**: `git diff --stat e341625..HEAD -- todos/ README.md scripts/`
> If any in-scope file changed since this plan was written, compare the
> "Current state" facts against the live files before proceeding; on a
> mismatch, treat it as a STOP condition. Another AI agent (Codex) may be
> working in this repo concurrently.

## Status

- **Priority**: P2
- **Effort**: S
- **Risk**: LOW
- **Depends on**: none
- **Category**: docs
- **Planned at**: commit `e341625`, 2026-07-11

## Why this matters

The numbered todo tracker (`todos/*.md`) encodes status twice — in the
filename (`NNN-<status>-<slug>.md`) and in YAML frontmatter (`status:`) — and
six files now disagree. `docs/RISKS.md:34` explicitly names this drift as a
standing project risk; it has materialized. Anyone (human or agent) reading
filenames re-opens finished work or skips open items. Separately, the README
front door says "v0.1" while the workspace ships 0.3.0, and promises
"stateless mode (June 2026)" — a date that has now passed. This plan makes the
tracker self-consistent, picks one authoritative status field, adds a guard
script, and fixes the README.

## Current state

The six contradictions, verified 2026-07-11 (filename token vs frontmatter
`status:`):

| File | Filename says | Frontmatter says |
|------|---------------|------------------|
| `todos/039-pending-p2-subscription-stale-after-route-refresh.md` | pending | `complete` |
| `todos/056-pending-p2-http-session-ux-parity.md` | pending | `done` |
| `todos/058-ready-p1-http-session-parity-program.md` | ready | `done` |
| `todos/059-ready-p1-daemon-owned-http-runtime.md` | ready | `done` |
| `todos/060-ready-p1-operator-recovery-ux-polish.md` | ready | `complete` |
| `todos/062-ready-p1-stale-oauth-runtime-recovery.md` | ready | (empty — no value after `status:`) |

The only genuinely open p1 is `todos/057-ready-p1-auth-oauth-hardening-program.md`
(frontmatter `status: ready` — consistent).

Note the tracker uses both `complete` and `done` as terminal markers
(38 files named `complete`, several frontmatters say `done`).

README staleness (README.md at commit e341625):
- `README.md:202` — `enable_prefix = true       # Legacy compatibility field; tool names are always prefixed in v0.1`
- `README.md:287` — `- Wire names are always prefixed in v0.1, regardless of enable_prefix`
- `README.md:317` — `7. **Future-proof** — MCP 2025-11-25, ready for stateless mode (June 2026)`
- Workspace version is `0.3.0` (`Cargo.toml:6`).

For todo 062 specifically: `docs/PROJECT-STATE-SNAPSHOT.md` (the repo's
canonical truth doc) records that stale-OAuth runtime recovery work landed on
`main` (see its "clearer operator auth/runtime UX" and PR #42–#50 zero-downtime
refresh entries, plus `docs/plans/2026-03-17-stale-oauth-runtime-recovery-plan.md`).
Read `todos/062-*.md` in full and check its acceptance criteria against the
snapshot before assigning a status.

Repo conventions: docs-only change; conventional commit `docs(todos): …` /
`docs(readme): …`. The repo's truth rules (`docs/TRUTH-RULES.md`) require
status claims to be verified against `main` — the snapshot doc is the
arbiter, not plan docs.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Enumerate drift | `for f in todos/*.md; do fm=$(grep -m1 '^status:' "$f" \| awk '{print $2}'); base=$(basename "$f"); tok=$(echo "$base" \| cut -d- -f2); [ "$fm" != "$tok" ] && echo "$base: name=$tok fm=$fm"; done` | lists mismatches (empty when done) |
| Tests (unchanged) | `cargo test --workspace` | all pass (docs-only change; run once at end) |

## Scope

**In scope**:
- The six `todos/*.md` files listed above (rename + frontmatter edit)
- `README.md` (three lines)
- `scripts/check-todo-status.sh` (create)

**Out of scope** (do NOT touch):
- Any other todo file's content — even if you notice other stale prose, only
  the six status contradictions are in scope.
- `docs/PROJECT-STATE-SNAPSHOT.md` — it is the truth source, not a target.
- `todos/057-*.md` — genuinely open; leave it.
- `.github/workflows/ci.yml` — the guard script stays optional/manual this round.

## Git workflow

- Branch: `docs/todo-status-reconciliation`
- Commits: `docs(todos): reconcile filename status with frontmatter`, `docs(readme): drop v0.1 wording and lapsed stateless date`, `chore(scripts): add todo status lint`.
- Do NOT push or open a PR unless the operator instructed it.

## Steps

### Step 1: Pick the authoritative field and normalize terminal statuses

Frontmatter `status:` is authoritative (machine-readable, harder to forget
than a rename). Normalize the terminal value to `complete` (the majority
convention in filenames): in the five files whose frontmatter says `done`,
change it to `complete` (`056`, `058`, `059`; leave `039` and `060` which
already say `complete`).

**Verify**: `grep -l '^status: done' todos/*.md` → no output.

### Step 2: Resolve todo 062's empty status

Read `todos/062-ready-p1-stale-oauth-runtime-recovery.md` in full. Check each
of its acceptance criteria against `docs/PROJECT-STATE-SNAPSHOT.md`. If all
are covered by snapshot entries, set `status: complete`. If any criterion is
NOT clearly landed on `main`, set `status: ready` (keeping it open) and note
which criterion is unmet in the file under a `## Status note (2026-07-11)`
heading.

**Verify**: `grep -m1 '^status:' todos/062-*.md` → prints a non-empty value.

### Step 3: Rename the mismatched files with `git mv`

Rename each file whose name token disagrees with its (now-normalized)
frontmatter, preserving the rest of the name:

```sh
git mv todos/039-pending-p2-subscription-stale-after-route-refresh.md todos/039-complete-p2-subscription-stale-after-route-refresh.md
git mv todos/056-pending-p2-http-session-ux-parity.md todos/056-complete-p2-http-session-ux-parity.md
git mv todos/058-ready-p1-http-session-parity-program.md todos/058-complete-p1-http-session-parity-program.md
git mv todos/059-ready-p1-daemon-owned-http-runtime.md todos/059-complete-p1-daemon-owned-http-runtime.md
git mv todos/060-ready-p1-operator-recovery-ux-polish.md todos/060-complete-p1-operator-recovery-ux-polish.md
# 062: rename to match whatever step 2 decided (complete → 062-complete-..., ready → leave name as-is)
```

**Verify**: run the "Enumerate drift" command from the table → the only
permitted output lines are files whose filename token is a non-status word
(check each manually); the six known contradictions no longer appear.

### Step 4: Add the guard script

Create `scripts/check-todo-status.sh` (mode +x), matching the style of
existing scripts in `scripts/` (bash, `set -euo pipefail`):

```bash
#!/usr/bin/env bash
# Fails if any todos/NNN-<status>-*.md filename disagrees with its frontmatter status.
set -euo pipefail
cd "$(dirname "$0")/.."
fail=0
for f in todos/[0-9]*.md; do
  base=$(basename "$f")
  tok=$(echo "$base" | cut -d- -f2)
  fm=$(grep -m1 '^status:' "$f" | awk '{print $2}' || true)
  if [ -z "$fm" ]; then echo "MISSING status: $base"; fail=1; continue; fi
  if [ "$fm" != "$tok" ]; then echo "MISMATCH: $base (name=$tok, frontmatter=$fm)"; fail=1; fi
done
exit $fail
```

**Verify**: `./scripts/check-todo-status.sh` → exit 0, no output.

### Step 5: Fix README

- `README.md:202`: change the comment to `# Legacy compatibility field; tool names are always prefixed` (drop `in v0.1`).
- `README.md:287`: change to `- Wire names are always prefixed in the current release, regardless of enable_prefix`.
- `README.md:317`: change to `7. **Future-proof** — MCP 2025-11-25, session-store seam ready for stateless operation` (removes the lapsed date without claiming stateless is implemented — only the seam exists, per `plug-core/src/session/mod.rs:113`).

**Verify**: `grep -n 'v0\.1\|June 2026' README.md` → no matches.

## Test plan

Docs-only change; no new Rust tests. The guard script (step 4) is the new
test. Final check: `cargo test --workspace` → all pass (proves no source file
was accidentally touched).

## Done criteria

- [ ] `./scripts/check-todo-status.sh` exits 0
- [ ] `grep -l '^status: done' todos/*.md` → no output
- [ ] `grep -n 'v0\.1\|June 2026' README.md` → no matches
- [ ] `git status` shows only in-scope files changed (six todos, README.md, the new script)
- [ ] `cargo test --workspace` exits 0
- [ ] `plans/README-claude-fable.md` status row updated

## STOP conditions

Stop and report back (do not improvise) if:

- Todo 062's acceptance criteria cannot be confidently matched against the snapshot (ambiguous = report, don't guess a status).
- You find MORE than the six listed contradictions (the tracker drifted further since planning — report the full list before renaming anything).
- Any todo file has frontmatter fields your rename would break (e.g. tooling that parses the filename — search `grep -rn 'todos/' scripts/ .github/` first; at planning time nothing parses these names).

## Maintenance notes

- Future todo status changes should edit frontmatter AND rename; the guard script catches drift. Consider wiring it into CI later (deliberately out of scope here).
- If the tracker grows another terminal synonym (`closed`, `shipped`), normalize early — the two-synonym state is how this drift started.
