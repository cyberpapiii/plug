# Plan 001: Close the CI/toolchain gaps — advisories gate, MSRV job, duplicate reqwest, fs2→fs4, 0700 token dirs

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README-claude-fable.md`.
>
> **Drift check (run first)**: `git diff --stat e341625..HEAD -- .github/workflows/ci.yml Cargo.toml plug-core/Cargo.toml plug-core/src/oauth.rs plug-core/src/downstream_oauth/mod.rs plug/src/daemon.rs`
> If any in-scope file changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding; on a
> mismatch, treat it as a STOP condition. Note: another AI agent (Codex) may
> be working in this repo concurrently — drift is a real possibility, check
> carefully.

## Status

- **Priority**: P1
- **Effort**: S
- **Risk**: LOW
- **Depends on**: none
- **Category**: dx / deps / security
- **Planned at**: commit `e341625`, 2026-07-11

## Why this matters

Five independent, small toolchain gaps found in the 2026-07-11 audit. CI's
documented RustSec advisory gate is a no-op (the deny job never runs
`advisories`), the declared MSRV of 1.86.0 is enforced by no job, an entire
duplicate reqwest 0.12 + TLS stack compiles into a size-gated binary purely
because of oauth2's default features (plug uses oauth2 for types only), the
unmaintained `fs2` crate (last release ~2019) guards the daemon-singleton and
token-file locks, and the OAuth token directories are created with
umask-inherited permissions while every file inside is carefully 0600. Each
fix is one file or a few lines; together they close the audit's whole
toolchain tier.

**Added 2026-07-11 (verified live, credit to the concurrent Codex audit):**
the clippy gate is ALREADY RED on current stable (1.97): two pre-existing
lints fail under `-D warnings` — `clippy::question_mark` at
`plug-core/src/artifacts.rs:482` (an `if let`/`else` block rewritable with
`?`) and `clippy::for_kv_map` at `plug-core/src/server/mod.rs:774`
(`for (name, _) in servers.iter()` → `for name in servers.keys()`). This is
toolchain drift (new-stable lints), not a code regression, but it blocks the
`cargo clippy … -D warnings` done-criterion of EVERY plan in this program.
Step 0 below fixes both mechanically; it must land before or with anything
else.

## Current state

Files and facts:

- `.github/workflows/ci.yml:15` — `env: MSRV: "1.86.0"` is declared and read by nothing; every job uses `dtolnay/rust-toolchain@stable`.
- `.github/workflows/ci.yml:93-101` — the deny job:

  ```yaml
  deny:
    name: cargo deny
    needs: check
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: EmbarkStudios/cargo-deny-action@v2
        with:
          command: check licenses bans sources
  ```

  No `advisories`. `deny.toml` at repo root has an `[advisories]` section
  nothing exercises. `CONTRIBUTING.md` tells contributors to run
  `cargo deny check advisories`.

- Workspace `Cargo.toml:101` — `oauth2 = "5"`. oauth2 5.0's default features
  are `[reqwest, rustls-tls]` (verified via `cargo info oauth2`). The
  workspace has ZERO uses of `oauth2::reqwest`, `oauth2::Client`, or
  `BasicClient` — only type imports (`TokenResponse`, `AccessToken`,
  `RefreshToken`, `basic::BasicTokenType`) in `plug-core/src/oauth.rs`,
  `plug-core/src/downstream_oauth/mod.rs`, `plug/src/daemon.rs`,
  `plug/src/commands/auth.rs`. `Cargo.lock` consequently carries BOTH
  `reqwest 0.12.28` (via oauth2) and `reqwest 0.13.3` (direct + rmcp).

- Workspace `Cargo.toml:90` — `fs2 = "0.4"`. Used in exactly two places:
  - `plug/src/daemon.rs:14` (`use fs2::FileExt as _;`) and `:425`
    (`file.try_lock_exclusive()`) — daemon singleton PID lock.
  - `plug-core/src/oauth.rs:393` (`use fs2::FileExt;`) and `:427`
    (`file.lock_exclusive()`) — OAuth token-file write lock.
  `fs4` is the maintained successor with the same advisory-locking purpose.

- `plug-core/src/oauth.rs:399` — inside the atomic JSON write helper:

  ```rust
  std::fs::create_dir_all(dir)
      .map_err(|e| AuthError::InternalError(format!("failed to create tokens dir: {e}")))?;
  ```

  No mode set. The token files themselves are opened 0600.

- `plug-core/src/downstream_oauth/mod.rs:510` — in `persist_state`:

  ```rust
  if std::fs::create_dir_all(dir).is_err() {
      return;
  }
  ```

  No mode set. State file written 0600 just below.

- The repo's own exemplar for 0700 dir creation is `plug/src/daemon.rs:385-395`:

  ```rust
  fn ensure_dir(path: &std::path::Path) -> anyhow::Result<()> {
      #[cfg(unix)]
      {
          use std::os::unix::fs::DirBuilderExt;
          std::fs::DirBuilder::new()
              .recursive(true)
              .mode(0o700)
              .create(path)
              .with_context(|| format!("failed to create directory: {}", path.display()))?;
      }
      #[cfg(not(unix))]
      ...
  }
  ```

Repo conventions: Rust 2024 workspace; conventional-commit messages
(`fix(config): …`, `chore(deps): …` — see `git log --oneline -20`); errors via
`thiserror`/`anyhow`; keep clippy clean with `-D warnings`.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Check | `cargo check --workspace` | exit 0 |
| Tests | `cargo test --workspace` | all pass |
| Lint | `cargo clippy --workspace --all-targets -- -D warnings` | exit 0 |
| Format | `cargo fmt --check` | exit 0 |
| Advisories | `cargo deny check advisories` | `advisories ok` |
| Dep tree | `cargo tree -i reqwest@0.12.28` | see step 3 |

## Scope

**In scope** (the only files you should modify):
- `.github/workflows/ci.yml`
- `Cargo.toml` (workspace root)
- `Cargo.lock` (regenerated by cargo, do not hand-edit)
- `plug-core/src/oauth.rs` (dir-creation line + fs2→fs4 import)
- `plug-core/src/downstream_oauth/mod.rs` (dir-creation line)
- `plug/src/daemon.rs` (fs2→fs4 import + lock-call adjustment only)
- `plug-core/src/artifacts.rs` (step 0: the `:482` clippy suggestion only)
- `plug-core/src/server/mod.rs` (step 0: the `:774` clippy suggestion only)

**Out of scope** (do NOT touch):
- `deny.toml` — its `[advisories]` config is already correct.
- Any other dependency bumps (rand, notify, etc.) — audited and rejected.
- The 0600 file-permission code — already correct.
- `plug-core/src/tls.rs`, TLS feature wiring — unrelated to the reqwest dedup.

## Git workflow

- Branch: `chore/toolchain-ci-quick-wins`
- Conventional commits, one per step, e.g. `ci: gate RustSec advisories in cargo-deny job`, `chore(deps): drop oauth2 default features to remove duplicate reqwest stack`.
- Steps are independent of each other after step 0; the operator may ask for them as separate PRs — the per-step commits make that split mechanical. In any split, step 0 lands first (it unblocks every plan's clippy gate).
- Do NOT push or open a PR unless the operator instructed it.

## Steps

### Step 0: Fix the two pre-existing clippy failures on stable 1.97

Apply clippy's own suggestions exactly (no other changes):

- `plug-core/src/artifacts.rs:482` — rewrite the `if let`/`else` chunk-suffix
  parse with the `?` operator as the `question_mark` lint suggests.
- `plug-core/src/server/mod.rs:774` — `for (name, _) in servers.iter()` →
  `for name in servers.keys()`.

**Verify**: `cargo clippy --workspace --all-targets -- -D warnings` → exit 0
(this is the program-wide gate; it must be green from here on).

### Step 1: Add `advisories` to the CI deny job

In `.github/workflows/ci.yml:101`, change:

```yaml
          command: check licenses bans sources
```

to:

```yaml
          command: check licenses bans sources advisories
```

**Verify**: `cargo deny check advisories` locally → `advisories ok` (it passes today; the change only makes CI enforce it).

### Step 2: Add an MSRV check job

> **Amendment (2026-07-12, execution round).** The original verify step fired
> its own STOP condition: `cargo +1.86.0 check --workspace` fails before
> compiling anything because six locked dependencies declare `rust-version`
> above the workspace's declared MSRV of 1.86.0 —
> `darling`/`darling_core`/`darling_macro` 0.23.0 (need 1.88, via
> `rmcp-macros`), `process-wrap` 9.1.0 (needs 1.87, via `rmcp`), and
> `time` 0.3.47/`time-core` 0.1.8 (need 1.88, via `tracing-appender`/`rcgen`).
> The declared MSRV was already unbuildable — a fiction, not a floor.
> Reviewer decision: align the declaration with reality rather than pin six
> crates backward. The step as executed: bump `rust-version` at workspace
> `Cargo.toml:8` AND `env.MSRV` at `.github/workflows/ci.yml:15` to
> `"1.88.0"`, then add the job below (its `${{ env.MSRV }}` reference picks
> up the new value; the job `name` should say 1.88.0). Verification becomes
> `cargo +1.88.0 check --workspace` → exit 0. The 1.86-based text below is
> retained for the record.

In `.github/workflows/ci.yml`, add a job after `check` (same shape as the
existing jobs, gated like `cross-check`):

```yaml
  msrv:
    name: MSRV check (1.86.0)
    needs: check
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@master
        with:
          toolchain: ${{ env.MSRV }}
      - uses: Swatinem/rust-cache@v2
      - run: cargo check --workspace
```

**Verify**: `rustup toolchain install 1.86.0 2>/dev/null && cargo +1.86.0 check --workspace` → exit 0. If toolchain 1.86.0 cannot be installed locally, verify YAML syntax only (`python3 -c "import yaml,sys; yaml.safe_load(open('.github/workflows/ci.yml'))"` → no error) and note that CI will be the real gate. If `cargo +1.86.0 check` FAILS, that is itself a finding — STOP and report which API breaks MSRV.

### Step 3: Drop oauth2 default features

In workspace `Cargo.toml:101`, change:

```toml
oauth2 = "5"
```

to:

```toml
oauth2 = { version = "5", default-features = false }
```

Run `cargo update -p oauth2 --precise 5.0.0` is NOT needed — just `cargo check --workspace` to regenerate the lockfile.

**Verify**:
1. `cargo check --workspace` → exit 0 (if it fails with missing oauth2 items, a type plug uses is behind a feature — STOP and report which).
2. `cargo tree -i reqwest@0.12.28` → error containing `package ID specification` / nothing depends on it (i.e. the 0.12 copy is GONE from the lockfile).
3. `grep -c 'name = "reqwest"' Cargo.lock` → `1`.
4. `cargo test --workspace` → all pass (OAuth unit + integration tests prove the types-only usage holds).

### Step 4: Replace fs2 with fs4

1. In workspace `Cargo.toml:90`, replace `fs2 = "0.4"` with the current fs4
   release with its std-sync feature (check `cargo info fs4` for the feature
   name; historically `sync`):

   ```toml
   fs4 = { version = "0.13", features = ["sync"] }
   ```

   (Adjust the version to the latest 0.x `cargo info fs4` reports.)
2. Update the two import sites:
   - `plug/src/daemon.rs:14`: `use fs2::FileExt as _;` → the fs4 equivalent (`use fs4::fs_std::FileExt as _;` in current fs4; check the crate docs if the path differs).
   - `plug-core/src/oauth.rs:393`: same substitution.
3. **API caution**: in fs4 ≥0.10, `try_lock_exclusive()` returns
   `io::Result<bool>` (Ok(false) on contention) instead of fs2's
   `io::Result<()>` (Err on contention). Inspect `plug/src/daemon.rs:425` —
   the surrounding code treats an `Err` as "another daemon holds the lock".
   If fs4's signature returns `bool`, rewrite that branch so `Ok(false)`
   takes the existing already-locked path and `Err(_)` remains a real I/O
   error. `lock_exclusive()` at `oauth.rs:427` keeps its blocking semantics —
   confirm its fs4 signature and adapt the `map_err` only if the types force it.

**Verify**: `cargo test --workspace` → all pass. The daemon singleton tests in `plug/src/daemon.rs` (test module from `:2637`) exercise the lock path — confirm specifically that tests matching `cargo test -p plug-mcp daemon` pass. Then `grep -rn 'fs2' Cargo.toml plug/src plug-core/src` → no matches.

### Step 5: Create secret directories 0700

Add a small shared helper in `plug-core` (suggested: new file
`plug-core/src/fs_perm.rs`, registered in `plug-core/src/lib.rs`, modeled
byte-for-byte on `plug/src/daemon.rs:385-395` `ensure_dir` shown above —
returning `std::io::Result<()>` so both call sites can map it into their own
error types):

```rust
pub fn ensure_dir_0700(path: &std::path::Path) -> std::io::Result<()> { ... }
```

Then:
- `plug-core/src/oauth.rs:399`: replace `std::fs::create_dir_all(dir)` with `crate::fs_perm::ensure_dir_0700(dir)` (keep the existing `.map_err(...)`).
- `plug-core/src/downstream_oauth/mod.rs:510`: replace `std::fs::create_dir_all(dir)` with `crate::fs_perm::ensure_dir_0700(dir)`.

Note: `ensure_dir_0700` sets 0700 only on directories it CREATES (DirBuilder
semantics) — do not add chmod-of-existing-dirs behavior; users may have
customized perms deliberately.

**Verify**: `cargo test --workspace` → all pass. Add the unit test from the Test plan below and confirm it passes: `cargo test -p plug-core fs_perm` → 1+ tests pass.

## Test plan

- New unit test in `plug-core/src/fs_perm.rs` (`#[cfg(test)] mod tests`,
  matching the inline-test convention used across plug-core):
  - creates a nested path under `std::env::temp_dir()`, calls
    `ensure_dir_0700`, asserts (unix-only, `#[cfg(unix)]`) that
    `metadata.permissions().mode() & 0o777 == 0o700` for the leaf dir, and
    that a second call on the existing dir is `Ok(())`.
- No new tests for steps 1–3 (CI/config changes; the existing suite is the gate).
- Step 4 relies on the existing daemon-singleton and OAuth-store tests.
- Verification: `cargo test --workspace` → all pass including the new test.

## Done criteria

- [ ] `cargo test --workspace` exits 0
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` exits 0
- [ ] `cargo fmt --check` exits 0
- [ ] `.github/workflows/ci.yml` deny command includes `advisories`; an `msrv` job exists referencing `env.MSRV`
- [ ] `grep -c 'name = "reqwest"' Cargo.lock` → `1`
- [ ] `grep -rn 'fs2' Cargo.toml plug/src plug-core/src` → no matches
- [ ] `grep -n 'create_dir_all' plug-core/src/oauth.rs plug-core/src/downstream_oauth/mod.rs` → no matches at the two former sites (other unrelated hits, if any, unchanged)
- [ ] No files outside the in-scope list modified (`git status`)
- [ ] `plans/README-claude-fable.md` status row updated

## STOP conditions

Stop and report back (do not improvise) if:

- `cargo check` after step 3 shows plug code using an oauth2 item that is feature-gated — do not re-enable features wholesale; report which item.
- fs4's locking API differs from both signatures described in step 4 (neither `Result<()>` nor `Result<bool>`), or the daemon singleton tests fail after the swap.
- `cargo +1.86.0 check --workspace` fails (MSRV is already broken — that's a separate finding, not something to fix here).
- `cargo deny check advisories` fails locally before your change (a new advisory landed — report it; do not add ignores).

## Maintenance notes

- The MSRV job means future dependency bumps must respect `rust-version = "1.86.0"` or bump it deliberately in both `Cargo.toml` and the CI env.
- If a future change starts actually using oauth2's HTTP client, it must re-enable the `reqwest` feature — reviewer should ask whether rmcp's reqwest (0.13) can serve instead.
- Reviewer should scrutinize the fs4 `try_lock_exclusive` contention branch in `daemon.rs` — getting Ok(false)/Err inverted would let two daemons run concurrently (the exact storm scenario the lock prevents).
