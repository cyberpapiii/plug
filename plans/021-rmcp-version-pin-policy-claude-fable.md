# Plan 021: Pin rmcp to `~1.7` so protocol-crate minor bumps are deliberate, reviewed upgrades

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README-claude-fable.md` — unless a reviewer dispatched you and
> told you they maintain the index.
>
> **Drift check (run first)**:
> `git diff --stat e341625..HEAD -- Cargo.toml Cargo.lock docs/CRATE-STACK.md`
> If any of these changed since this plan was written, compare the "Current
> state" excerpts against the live files before proceeding; on a mismatch,
> treat it as a STOP condition.

## Status

- **Priority**: P2
- **Effort**: S
- **Risk**: LOW
- **Depends on**: plans/001-toolchain-ci-quick-wins-claude-fable.md (soft —
  001 also edits the workspace `Cargo.toml`; land 001 first to avoid a
  pointless merge conflict. Do not run the two in parallel.)
- **Category**: migration (dependency policy)
- **Planned at**: commit `e341625`, 2026-07-12

## Why this matters

`rmcp` is THE protocol crate: plug's downstream server handlers, upstream
client sessions, transports, auth, and elicitation all sit on its types and
negotiated protocol behavior. The workspace declares it as
`version = "1.7.0"`, which in Cargo is a caret requirement (`^1.7.0` — any
`1.x` satisfies it). The committed `Cargo.lock` protects `--locked` builds,
and every documented install command uses `--locked` — but three paths still
float: (1) a user running `cargo install plug-mcp` *without* `--locked`
resolves rmcp to the newest `1.x` at install time; (2) an in-repo broad
`cargo update` silently moves the lockfile to the newest `1.x`; (3) the
published `plug-core`/`plug-mcp` crates carry `^1.7.0` to crates.io. For a
fast-moving protocol SDK, an unreviewed minor bump can change negotiated
protocol versions or transport semantics under a project whose stated
posture is "reliability over protocol surface area" (CLAUDE.md). Tightening
to `~1.7` (>=1.7.0, <1.8.0) lets patch fixes flow while making a `1.8`
adoption a deliberate manifest edit that goes through review and the full
test suite — which is exactly how the 1.7.0 bump itself was done (todo 068,
closed in the 2026-07-03 hardening batch).

## Current state

- `Cargo.toml` (workspace root) — the single rmcp declaration, lines 14–34:

  ```toml
  # Cargo.toml:14-16 (feature list continues to line 34)
  # MCP SDK
  rmcp = { version = "1.7.0", features = [
      "client",
  ```

  The features array lists 13 entries ending with `"auth"` at line 33 and
  `] }` at line 34. **Only the `version` value changes in this plan — the
  features list must remain byte-identical.**
- `plug-core/Cargo.toml:12`, `plug/Cargo.toml:17`,
  `plug-test-harness/Cargo.toml:18` — all say `rmcp.workspace = true`
  (inherit from the root; no edits needed there).
- `Cargo.lock` — committed (deliberately, so `cargo install --git … --locked`
  is reproducible; see `docs/hardening-log.md:504`). It currently resolves
  `rmcp` to exactly `1.7.0` from crates.io. Since `1.7.0` satisfies `~1.7`,
  this plan requires **no lockfile change**.
- `docs/CRATE-STACK.md:10-11` — the dependency-rationale doc; current entry:

  ```markdown
  - `rmcp` 1.7.0
    MCP protocol implementation for both downstream server handlers and upstream client sessions.
  ```

- Publishing context (from `docs/audit-2026-05-17.md:630` and
  `docs/OPERATOR-GUIDE.md:208`): `plug-core` and `plug-mcp` are published on
  crates.io; the public install command is
  `cargo install plug-mcp --locked`. `plug-test-harness` is
  `publish = false`. A `~1.7` requirement is narrower for downstream
  consumers of the published crates than `^1.7.0`, which is acceptable:
  `plug-core`'s only real consumer is `plug-mcp`.
- No other file hard-codes an rmcp version: `.github/workflows/*.yml` has no
  rmcp references; `grep -rn '"1\.7' Cargo.toml plug*/Cargo.toml` matches
  only `Cargo.toml:15`.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Build check | `cargo check` | exit 0 |
| Tests | `cargo test --workspace` | all pass |
| Lint | `cargo clippy --workspace --all-targets -- -D warnings` | exit 0 (see done-criteria caveat) |
| Format | `cargo fmt --check` | exit 0 |
| Lock inspection | `git diff Cargo.lock` | empty output |

## Scope

**In scope** (the only files you should modify):
- `Cargo.toml` (workspace root) — one token: the rmcp `version` value.
- `docs/CRATE-STACK.md` — the rmcp entry gains a one-line version policy.

**Out of scope** (do NOT touch, even though they look related):
- `Cargo.lock` — must not change (STOP condition if it wants to).
- The rmcp `features` array — unchanged.
- Other dependency requirements (`oauth2`, `fs2`, etc.) — plan 001 owns
  those; `rustls = "0.23"` is a 0.x caret (already minor-locked by Cargo's
  0.x caret semantics) and needs nothing.
- README/OPERATOR-GUIDE install commands — already `--locked`; unchanged.

## Git workflow

- Branch: `advisor/021-rmcp-version-pin` off `main`.
- One commit, conventional style matching repo history (e.g.
  `fix(ci): satisfy linux clippy for codesign doctor`, `docs(snapshot): …`):
  suggested message: `build(deps): pin rmcp to ~1.7 and document the upgrade policy`.
- Do NOT push or open a PR unless the operator instructed it.

## Steps

### Step 1: Tighten the requirement

In the workspace root `Cargo.toml`, change line 15:

```toml
rmcp = { version = "1.7.0", features = [
```

to:

```toml
rmcp = { version = "~1.7", features = [
```

Nothing else on that line or in the features array changes.

**Verify**: `cargo check` → exit 0, AND `git diff Cargo.lock` → empty
(1.7.0 already satisfies `~1.7`, so the resolver must not touch the lock).

### Step 2: Document the policy where the crate is described

In `docs/CRATE-STACK.md`, extend the rmcp entry (currently lines 10–11) to:

```markdown
- `rmcp` `~1.7` (resolved: 1.7.0)
  MCP protocol implementation for both downstream server handlers and upstream client sessions.
  Version policy: tilde-pinned. Patch releases flow; a minor bump (1.8+) is a
  deliberate manifest edit that must go through review and the full workspace
  suite, because rmcp minors can change negotiated protocol behavior. The
  1.7.0 adoption itself was such a reviewed bump (todo 068).
```

**Verify**: `grep -n '~1.7' docs/CRATE-STACK.md` → one match in the rmcp
entry.

### Step 3: Run the full gates

**Verify**: `cargo test --workspace` → all pass;
`cargo fmt --check` → exit 0;
`cargo clippy --workspace --all-targets -- -D warnings` → exit 0 *if plan
001 step 0 has landed* (see done criteria).

## Test plan

No new tests — this is a manifest-requirement and docs change with an
intentionally empty runtime diff (the resolved dependency graph is
byte-identical, proven by the empty `Cargo.lock` diff). The full existing
suite runs as the gate.

## Done criteria

Machine-checkable. ALL must hold:

- [ ] `grep -c 'version = "~1.7"' Cargo.toml` → 1, and
      `grep -c 'version = "1.7.0"' Cargo.toml` → 0
- [ ] `git diff Cargo.lock` → empty
- [ ] `cargo test --workspace` exits 0
- [ ] `cargo fmt --check` exits 0
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` exits 0.
      **Known pre-existing failure caveat**: at the planned-at commit, this
      gate is RED for two findings unrelated to this plan (`question_mark`
      at `plug-core/src/artifacts.rs:482`, `for_kv_map` at
      `plug-core/src/server/mod.rs:774` — plan 001 step 0 fixes them). If
      clippy fails with EXACTLY those two and nothing touching this plan's
      files, record that in the status row and treat this criterion as met.
- [ ] `git status` shows only `Cargo.toml` and `docs/CRATE-STACK.md` modified
- [ ] `plans/README-claude-fable.md` status row updated

## STOP conditions

Stop and report back (do not improvise) if:

- After step 1, `cargo check` wants to modify `Cargo.lock` (would mean the
  lock no longer holds rmcp 1.7.x — the tree drifted; the pin decision needs
  re-review against whatever version is actually locked).
- `Cargo.toml`'s rmcp entry no longer matches the excerpt (someone bumped or
  restructured it since e341625).
- Any test failure appears — the diff is supposed to be resolution-neutral,
  so a failure means something else is broken; do not chase it here.

## Maintenance notes

- When a deliberate rmcp 1.8 upgrade happens: change `~1.7` → `~1.8`, run
  `cargo update -p rmcp`, review the rmcp changelog for protocol-version and
  transport changes, and run the full parity matrix — then update the
  CRATE-STACK entry's "resolved:" note.
- Reviewer should scrutinize: the `Cargo.lock` diff is empty, and the
  features array is untouched.
- Deliberately NOT done here: exact-pinning (`=1.7.0`) — it blocks patch
  fixes and is hostile to downstream consumers of the published `plug-core`;
  and a CI lockfile-drift guard — redundant once the requirement itself
  blocks minor floats.
