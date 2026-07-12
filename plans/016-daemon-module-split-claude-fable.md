# Plan 016: Decompose daemon.rs (~4,990 lines incl. tests) into focused submodules — move-only

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README-claude-fable.md`.
>
> **Drift check (run first)**: `git diff --stat e341625..HEAD -- plug/src/daemon.rs`
> This file WILL have drifted if plans 003 (busy-spin/idle-select fixes) and
> 015 (fanout dedup) landed — that is expected and fine; the module
> boundaries below are anchored to function names, not line numbers. Re-find
> each anchor by name. If a listed anchor function no longer exists, STOP.
> Another AI agent (Codex) may be working in this repo concurrently.

## Status

- **Priority**: P3
- **Effort**: M/L
- **Risk**: MEDIUM (large mechanical change; "move-only" discipline is the mitigation)
- **Depends on**: **Plan 015 SHOULD land first** (it thins the fanout block
  this plan moves). Plans 003 and 014 also touch daemon.rs — land before, or
  coordinate. If 015 has NOT landed, STOP and confirm with the operator
  before proceeding (moving the fat fanout block makes 015's diff much worse).
- **Category**: tech debt
- **Planned at**: commit `e341625`, 2026-07-11

## Why this matters

`plug/src/daemon.rs` is 4,987 lines (≈2,640 production + ≈2,350 test lines;
the test module starts at `:2637`) holding at least seven distinguishable
responsibilities: client registry, runtime-dir/socket setup, wire framing,
the per-client connection loop, notification fanout, auth-status dispatch,
and the MCP request dispatcher. It is the repo's #1 merge-conflict hotspot
(plans 003, 014, 015 all touch it in this program alone) and the hardest file
to review. Every structural boundary is already visible in the code — this
split is pure mechanics, deliberately separated from ALL behavior changes so
review is "moves compile and tests pass", nothing else.

**HARD RULE for this plan: move-only.** No logic edits, no signature changes
beyond visibility (`pub(crate)`/`pub(super)`), no renames, no "while I'm
here" cleanups. Any real fix found along the way is reported, not made.

## Current state

Verified boundaries at commit `e341625` (re-anchor by NAME at execution
time):

| Anchor (verified at e341625) | Responsibility | Target module |
|---|---|---|
| `ClientRegistry` (struct, `:46`) + its impls | who's connected, per-client state | `daemon/registry.rs` |
| `ensure_dir` (`:385`, the 0o700 DirBuilder helper) + runtime-dir/socket-path setup around it | filesystem/socket bootstrap | `daemon/paths.rs` (note: plan 001 may have moved `ensure_dir`-equivalent logic into `plug-core/src/fs_perm.rs` — if so, this module shrinks; keep whatever daemon-specific setup remains) |
| `FrameReader` (`:943`) + frame write helpers near it | wire framing (length-prefix, chunking) | `daemon/framing.rs` |
| `send_ipc_control_notification` (`:1310`) + the fanout block (`:1321-1420`, post-015: thin classify/resolve calls + IPC delivery) | notification delivery to IPC clients | `daemon/notify.rs` |
| `dispatch_auth_status` (`:1957`) | auth-status IPC surface | `daemon/auth_status.rs` |
| `dispatch_mcp_request` (`:2172-2533`) | the big MCP method dispatcher | `daemon/mcp_dispatch.rs` |
| Connection accept loop, per-client task, reverse-request plumbing (the `:1043-1204` region incl. `handle_reverse_request`; post-003 shape) | session/connection lifecycle | stays in `daemon/mod.rs` (the spine) |
| `mod tests` (`:2637`) | tests | split alongside what they test (see step 5) |

`daemon.rs` becomes directory module `plug/src/daemon/mod.rs` re-exporting
the current public surface unchanged (check what `main.rs`/`runtime.rs`/
`ipc_proxy.rs` import from `daemon::` — `grep -rn 'daemon::' plug/src | grep -v 'daemon/'` — and preserve every path via `pub use`).

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Fast gate per move | `cargo check -p plug-mcp` | exit 0 |
| Daemon tests | `cargo test -p plug-mcp daemon` | all pass |
| Full tests | `cargo test --workspace` | all pass |
| Lint | `cargo clippy --workspace --all-targets -- -D warnings` | exit 0 |
| Format | `cargo fmt --check` | exit 0 |
| Move-only audit | `git log --oneline --stat` + reviewing each commit's diff | additions ≈ deletions per move |

## Scope

**In scope**:
- `plug/src/daemon.rs` → `plug/src/daemon/{mod,registry,paths,framing,notify,auth_status,mcp_dispatch}.rs`.
- Minimal visibility adjustments (`pub(crate)`, `pub(super)`) and import updates in the moved code and its callers.

**Out of scope** (do NOT touch):
- ANY logic, error message, log line, or constant.
- Public API of the daemon module as seen from the rest of the crate (preserve via re-exports).
- `ipc_proxy.rs`, `runtime.rs` beyond import-path updates IF any are needed (re-exports should make them unnecessary — prefer zero edits there).
- Renaming functions/types "for clarity".

## Git workflow

- Branch: `refactor/daemon-module-split`
- Commits: ONE MODULE PER COMMIT, in the step-3 order, each commit green on
  `cargo check -p plug-mcp` + `cargo test -p plug-mcp daemon`. Message:
  `refactor(daemon): move <responsibility> to daemon/<mod>.rs (move-only)`.
- Do NOT push or open a PR unless the operator instructed it.

## Steps

### Step 1: Re-anchor and inventory the public surface

Find every anchor from the table by name. List everything the rest of the
crate imports from `daemon::` (grep above). Note where plans 003/015 changed
the regions you're moving (their diffs are part of what you move — verbatim).

**Verify**: all anchors found; import list recorded.

### Step 2: Create the directory module

`git mv plug/src/daemon.rs plug/src/daemon/mod.rs`. Compile.

**Verify**: `cargo check -p plug-mcp` → exit 0 with zero other edits (if the
move alone breaks paths, fix `mod` declaration in `lib.rs`/`main.rs` only).
Commit.

### Step 3: Move one module at a time, dependency-leaves first

Order (least-entangled first): `framing` → `paths` → `registry` →
`auth_status` → `notify` → `mcp_dispatch`. For each:

1. Cut the anchor items + their private helpers (helpers used ONLY by the
   moved items go along; helpers shared with the spine stay in `mod.rs` with
   `pub(super)`).
2. Paste into the new file verbatim; add the needed `use` lines; add
   `pub(super)`/`pub(crate)` where the spine calls in.
3. `mod <name>;` + any `pub use` in `mod.rs` to keep external paths stable.

**Verify (per move)**: `cargo check -p plug-mcp` → exit 0;
`cargo test -p plug-mcp daemon` → all pass. Commit before the next move.

### Step 4: Spine cleanup pass (still move-only)

`mod.rs` should now contain: module decls + re-exports, the daemon entry
point, the accept loop, the per-client connection task, reverse-request
plumbing. If anything else survived, either it's shared glue (fine — note
it) or a missed move (do it).

**Verify**: `wc -l plug/src/daemon/*.rs` — no file > ~900 lines, `mod.rs`
ideally < ~700; record the numbers.

### Step 5: Split the tests

Move each test to the module owning what it tests (`#[cfg(test)] mod tests`
per file); tests exercising the whole loop stay in `mod.rs`'s test module.
Test bodies verbatim; only imports change. Test count before == after
(`cargo test -p plug-mcp daemon 2>&1 | grep 'test result'` — compare totals
against a pre-split run you record in step 1).

**Verify**: `cargo test -p plug-mcp daemon` → same count, all pass; then full
gates: `cargo test --workspace`, clippy, fmt → green.

## Test plan

No new tests — the invariant is EXISTING tests unchanged (bodies verbatim)
and their count identical. That, plus per-commit green checks, is the move-
only proof.

## Done criteria

- [ ] `cargo test --workspace` exits 0; clippy/fmt gates exit 0
- [ ] Test count identical pre/post split (recorded numbers in the report)
- [ ] Each commit is a single module move, individually green
- [ ] `grep -rn 'daemon::' plug/src | grep -v 'src/daemon/'` — all pre-existing import paths still resolve (zero caller edits, or listed if a re-export couldn't cover one)
- [ ] Per-file line counts recorded; no behavior/log/error-string diffs (spot-check: `git diff e341625..HEAD -- plug/src/daemon* | grep '^[+-]' | grep -v '^[+-][+-]' | grep -iv 'use \|mod \|pub(super)\|pub(crate)\|^[+-]$' | head -50` — surviving hits should be only moved-verbatim lines appearing as -/+ pairs)
- [ ] `plans/README-claude-fable.md` status row updated

## STOP conditions

Stop and report back (do not improvise) if:

- Plan 015 has not landed (see Depends on) — confirm with the operator before moving the fat fanout block.
- An anchor function from the table no longer exists by name — the file was restructured by someone else (possibly Codex); re-survey and report before moving anything.
- A move requires an actual code change (not visibility/imports) to compile — e.g. a private type is captured in a closure crossing module lines in a way that needs restructuring — report it; that item stays in the spine this round.
- Any test needs its BODY (not imports) edited to pass after a move — that's behavior drift; revert the move and report.
- Merge conflicts with concurrent work (Codex) mid-split — stop at the last green commit and report rather than resolving conflicts inside a half-moved file.

## Maintenance notes

- Future daemon features go in the owning submodule; `mod.rs` growing again is the regression signal (add that one line to the module doc comment).
- The re-export shim in `mod.rs` can be slimmed later by updating callers to the new paths — deliberately NOT this plan (zero-caller-edit discipline).
- Plan 017's dispatch-unification design will likely propose merging `mcp_dispatch.rs` with the shared dispatcher — this split makes that diff reviewable; reference `daemon/mcp_dispatch.rs` in that design.
