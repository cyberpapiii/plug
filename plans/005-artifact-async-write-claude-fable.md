# Plan 005: Move large artifact payload writes off the async runtime threads

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README-claude-fable.md`.
>
> **Drift check (run first)**: `git diff --stat e341625..HEAD -- plug-core/src/artifacts.rs`
> If the file changed since this plan was written, compare the "Current
> state" excerpts against the live code before proceeding; on a mismatch,
> treat it as a STOP condition. Another AI agent (Codex) may be working in
> this repo concurrently.

## Status

- **Priority**: P3
- **Effort**: S → M (re-scoped; see amendment)
- **Risk**: LOW → MEDIUM (async signature ripple)
- **Depends on**: plans 004 and 019 merged first (they own `proxy/mod.rs` / `proxy/tasks.rs`)
- **Category**: perf
- **Planned at**: commit `e341625`, 2026-07-11

> **Reviewer re-scope amendment (2026-07-12, after execution STOP).** The first
> execution attempt STOPPED correctly on STOP condition 2: this plan assumed the
> spill functions were async, and they are not. Verified facts (reviewer re-checked
> each): `maybe_spill_tool_result` (`plug-core/src/artifacts.rs:76`),
> `maybe_spill_tool_result_with_limit` (`:84`), and `maybe_spill_task_payload`
> (`:179`) are all plain sync `fn`; there is NO existing `spawn_blocking` usage
> anywhere in `plug-core/src` or `plug/src` (the "Commands you will need" claim of
> an existing idiom is wrong); the four call sites are
> `plug-core/src/proxy/mod.rs` (`maybe_spill_tool_result`, inside the async block
> built by `call_tool_inner`) and `plug-core/src/proxy/tasks.rs` (three
> `maybe_spill_task_payload` calls inside `async fn` bodies). Locate call sites by
> symbol name, not line number — plans 004 and 019 will have edited these files.
>
> **Revised approach (authorized):** make all three spill functions `async fn`;
> inside `maybe_spill_tool_result_with_limit`, move ONLY the blocking filesystem
> write section into `tokio::task::spawn_blocking` (clone/move the owned data it
> needs; `.await` the join handle and map `JoinError` into the existing `McpError`
> path); `.await` the three public-call sites listed above plus any the compiler
> then surfaces. **Expanded scope:** `plug-core/src/proxy/mod.rs` and
> `plug-core/src/proxy/tasks.rs` are now in scope for signature-adaptation edits
> ONLY (adding `.await`, no logic changes). Everything else in the original Scope
> section still stands. New STOP condition: if any spill call site turns out to be
> in a context that cannot become async (e.g. a `Drop` impl or sync trait method),
> STOP and report.

## Why this matters

This closes a residual explicitly tracked since PR #58 in
`docs/PROJECT-STATE-SNAPSHOT.md` ("the ≥16MB artifact write is still
synchronous (not yet `spawn_blocking`)"). When an oversized tool result is
artifactized, the payload — potentially tens of MB — is written with
synchronous `std::fs::write` on a tokio worker thread, stalling every other
task scheduled on that thread (notification fanout, other in-flight calls)
for the duration of the disk write. Same for the base64-decoded attachment
materialization. Moving the blocking filesystem work to `spawn_blocking` is
the standard tokio remedy.

## Current state

`plug-core/src/artifacts.rs` — the artifactize path (inside the store's
async result-handling method; results only reach here when larger than
`INLINE_RESULT_MAX_BYTES`):

```rust
// artifacts.rs:108-119
let id = uuid::Uuid::new_v4().simple().to_string();
let artifact_dir = self.base_dir.join(&id);
std::fs::create_dir_all(&artifact_dir)
    .map_err(|e| McpError::internal_error(e.to_string(), None))?;

let payload_path = artifact_dir.join(PAYLOAD_FILE);
std::fs::write(&payload_path, &serialized)
    .map_err(|e| McpError::internal_error(e.to_string(), None))?;

let created_at = SystemTime::now();
let materialized_path = maybe_materialize_attachment(&artifact_dir, &result)
    .map_err(|e| McpError::internal_error(e.to_string(), None))?;
```

`maybe_materialize_attachment` (helper lower in the file) does the second
large write (`:551-553`):

```rust
let attachment_path = artifact_dir.join(sanitize_filename(filename));
std::fs::write(&attachment_path, bytes)?;
Ok(Some(attachment_path))
```

Facts you need:
- Confirm the enclosing function of the first excerpt is `async fn` (it is
  reached from async tool-call handling; check its signature at the top of
  the impl block containing line ~90).
- `serialized: Vec<u8>` and `result` are owned/available at that point;
  `maybe_materialize_attachment` takes `(&Path, &CallToolResult)` and returns
  `anyhow::Result<Option<PathBuf>>`.
- The metadata write below (`write_metadata`, `:565+`) is small JSON — moving
  it is optional; the payload + attachment writes are the point.
- Test module in the same file (from `:700+`) writes fixture artifacts with
  `std::fs::write` directly — those are tests, leave them.

Conventions: errors are mapped to `McpError::internal_error(e.to_string(), None)`
at this layer; keep that. tokio is the only runtime; `tokio::task::spawn_blocking`
is already used elsewhere in the workspace (`grep -rn 'spawn_blocking' plug-core/src plug/src`
to see the local idiom for JoinError mapping).

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Check | `cargo check --workspace` | exit 0 |
| Artifact tests | `cargo test -p plug-core artifacts` | all pass |
| Full tests | `cargo test --workspace` | all pass |
| Lint | `cargo clippy --workspace --all-targets -- -D warnings` | exit 0 |
| Format | `cargo fmt --check` | exit 0 |

## Scope

**In scope**:
- `plug-core/src/artifacts.rs` — the artifactize write path only.

**Out of scope** (do NOT touch):
- The chunked `resources/read` path (`read_chunk_text` etc.) — reads are bounded to one chunk since PR #58.
- Cache pruning / eviction logic.
- `INLINE_RESULT_MAX_BYTES`, retention, or any threshold constants.
- Test-fixture `std::fs::write` calls inside `#[cfg(test)]`.

## Git workflow

- Branch: `perf/artifact-spawn-blocking`
- Commit: `perf(artifacts): write oversized payloads via spawn_blocking`.
- Do NOT push or open a PR unless the operator instructed it.

## Steps

### Step 1: Bundle the blocking filesystem work into one `spawn_blocking`

Restructure the excerpt at `:108-119` so dir creation, payload write, and
attachment materialization run in a single blocking task (one hop, not
three). The closure needs owned data: move `artifact_dir` (PathBuf),
`serialized` (Vec<u8>), and either the `result` (if cheap to clone — it was
just serialized, so prefer passing the needed parts) or restructure
`maybe_materialize_attachment` to take owned extracted inputs. Acceptable
shape:

```rust
let artifact_dir_cloned = artifact_dir.clone();
let result_for_attach = result.clone(); // CallToolResult: Clone — verify; if not derivable cheaply, extract the attachment fields before the closure instead
let payload_path = artifact_dir.join(PAYLOAD_FILE);
let payload_path_cloned = payload_path.clone();
let (materialized_path,) = tokio::task::spawn_blocking(move || -> anyhow::Result<(Option<PathBuf>,)> {
    std::fs::create_dir_all(&artifact_dir_cloned)?;
    std::fs::write(&payload_path_cloned, &serialized)?;
    let materialized = maybe_materialize_attachment(&artifact_dir_cloned, &result_for_attach)?;
    Ok((materialized,))
})
.await
.map_err(|e| McpError::internal_error(e.to_string(), None))?   // JoinError
.map_err(|e| McpError::internal_error(e.to_string(), None))?;  // io/anyhow
```

Watch two things: (a) `serialized` is used earlier for `size` and later for
`chunk_count` (`serialized.len().div_ceil(...)` at `:128`) — capture
`serialized.len()` into a local BEFORE moving the Vec into the closure and
use that local afterward; (b) `result` is returned at the end of the happy
path in some branches and used by `build_preview(&result)` (`:129`) — clone
only what the closure needs, don't move `result` itself.

**Verify**: `cargo check --workspace` → exit 0.

### Step 2: Run the artifact test suite

The existing tests in this file cover store/read/eviction round-trips and
will exercise the new path.

**Verify**: `cargo test -p plug-core artifacts` → all pass; then `cargo test --workspace` → all pass.

## Test plan

- The existing artifact round-trip tests are the primary guard (the write
  path is behavior-identical, just rescheduled).
- Add ONE new test only if the file lacks a direct "oversized result becomes
  an artifact with an attachment" round-trip (check for a test invoking the
  store with an attachment-shaped result; if present, no new test needed —
  say so in the completion report).
- Verification: `cargo test -p plug-core artifacts` → all pass.

## Done criteria

- [ ] `cargo test --workspace` exits 0
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` exits 0
- [ ] `cargo fmt --check` exits 0
- [ ] Non-test `std::fs::write`/`create_dir_all` calls in the artifactize path are inside `spawn_blocking` (`grep -n 'std::fs::write\|create_dir_all' plug-core/src/artifacts.rs` — remaining non-test hits are `write_metadata` (optional) and test fixtures)
- [ ] No files outside the in-scope list modified (`git status`)
- [ ] `plans/README-claude-fable.md` status row updated

## STOP conditions

Stop and report back (do not improvise) if:

- `CallToolResult` is not `Clone` AND `maybe_materialize_attachment`'s inputs can't be cheaply extracted before the closure — report the ownership tangle instead of cloning multi-MB buffers twice.
- The enclosing function turns out NOT to be async (then `spawn_blocking` is wrong and the finding itself needs re-evaluation — report).
- Any artifact test fails in a way that suggests ordering mattered (e.g. a test observed the dir existing before the record) — report rather than reordering test expectations.

## Maintenance notes

- If artifact encryption or compression is ever added, it belongs inside this same `spawn_blocking` closure.
- `write_metadata` (small JSON) deliberately stays synchronous; if metadata grows, move it in too.
- This closes the PR #58 residual — after merge, update the residual note in `docs/PROJECT-STATE-SNAPSHOT.md` per the repo's post-merge truth pass checklist (`CLAUDE.md`).
