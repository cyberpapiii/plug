# Plan 011: Interlock reconnects/restarts with config reloads so a removed server cannot be resurrected

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README-claude-fable.md`.
>
> **Drift check (run first)**: `git diff --stat e341625..HEAD -- plug-core/src/engine.rs plug-core/src/server/mod.rs plug-core/src/reload.rs plug-core/src/config/mod.rs`
> If any of these changed since this plan was written, compare the "Current
> state" excerpts against the live code before proceeding; on a mismatch,
> treat it as a STOP condition. Another AI agent (Codex) may be working in
> this repo concurrently.

## Status

- **Priority**: P2
- **Effort**: M
- **Risk**: MEDIUM (supervision/reload interaction; needs careful lock discipline)
- **Depends on**: none
- **Category**: correctness
- **Planned at**: commit `e341625`, 2026-07-11
- **Version**: v2.1, 2026-07-11. v1 proposed validating inside
  `replace_server` "under its existing write lock" and comparing against the
  ArcSwap config — both premises were wrong (no such lock exists; reload
  swaps the config only AFTER its stop/start work; and `replace_server` has
  a second production caller v1 missed). v2.1 additionally corrected the
  comparison contract: whole-struct equality (v2's `PartialEq` derive) would
  discard good reconnections on changes reload itself classifies as
  NON-material — the commit check now shares reload's own
  `server_config_changed` predicate; a v2.2 pass (2026-07-12) extended the
  same predicate to the retry-loop early-exit, which still said "equals".
  Credit for all rounds: concurrent cross-agent review. This version is the
  design of record.

## Why this matters

Two engine paths connect an upstream from a config snapshot and then
**unconditionally** install it into the live server map via
`replace_server`:

- `Engine::do_reconnect` (`plug-core/src/engine.rs:576`) — supervision
  recovery; snapshots the server's config, retries the connect up to
  `RECONNECT_RETRY_MAX_ATTEMPTS` times with exponential backoff (a window of
  tens of seconds), then installs.
- `Engine::restart_server` (`plug-core/src/engine.rs:481`) — the manual
  restart command (called from the daemon at `plug/src/daemon.rs:1521` and
  `:2102`); smaller window (no retry loop), same shape.

Meanwhile the config hot-reload path (`Engine::reload_config`, guarded by
`Engine::reload_lock`) can remove or rewrite that same server. Neither
installing path holds or checks `reload_lock`. Sequence:

1. Server `foo` becomes unhealthy → `do_reconnect("foo")` starts with a
   snapshot of foo's OLD config.
2. Operator deletes `foo` from `config.toml` (or changes its URL/auth) →
   reload stops/removes it and swaps in the new config.
3. `do_reconnect` succeeds against the OLD config → `replace_server`
   unconditionally inserts → a deleted server is resurrected (running,
   routable, with stale config/credentials), or a reconfigured server is
   clobbered back to its old settings.

Related smaller defect in the same code: `replace_server` retires the old
upstream via a fire-and-forget 30-second grace `tokio::spawn` — on engine
shutdown these orphaned tasks keep old connections alive past
`shutdown_all`. That residual is plan 012's scope; do NOT fix it here, but
don't make it worse.

## Current state

Verified at commit `e341625`. All excerpts below are real code, abbreviated.

**The two installing callers** (the ONLY production callers of
`replace_server` — re-verify with
`grep -rn 'replace_server' plug-core/src plug/src`):

`plug-core/src/engine.rs:576-629` — `do_reconnect`:

```rust
async fn do_reconnect(&self, server_id: &str) -> Result<(), anyhow::Error> {
    let config = self.config.load();
    let server_config = config.servers.get(server_id)
        .ok_or_else(|| anyhow::anyhow!("unknown server: {server_id}"))?
        .clone();                                     // snapshot taken ONCE

    let mut attempt = 1;
    let mut delay = RECONNECT_RETRY_MIN_DELAY;
    let upstream = loop {
        match self.server_manager.start_server(server_id, &server_config).await {
            Ok(upstream) => break upstream,
            Err(e) if attempt < RECONNECT_RETRY_MAX_ATTEMPTS
                && is_retryable_reconnect_error(&e) => {
                tokio::time::sleep(delay).await;      // config can change here
                attempt += 1;
                delay = (delay * 2).min(RECONNECT_RETRY_MAX_DELAY);
            }
            Err(e) => { /* emit EngineEvent::Error */ return Err(e); }
        }
    };

    self.server_manager.replace_server(server_id, upstream).await;  // UNCONDITIONAL
    self.tool_router.refresh_tools().await;
    let _ = self.event_tx.send(EngineEvent::ServerStarted { .. });
    tracing::info!(server = %server_id, "server reconnected");
    Ok(())
}
```

`do_reconnect` is reached only through `Engine::reconnect_server` (`:552`),
which claims a per-server `AtomicBool` (RAII-cleared) so reconnects don't
stampede — that guard stays as-is.

`plug-core/src/engine.rs:481-540` — `restart_server`: rate-limit check
(10s cooldown per server), `config.load()` + snapshot clone, emit
`ServerStopped`, then `start_server` → on Ok: `replace_server` →
`sync_refresh_loop_for_server` → `refresh_tools` → emit `ServerStarted`.
Same unconditional install, no retry loop.

**`replace_server` holds no lock** — `plug-core/src/server/mod.rs:1743-1768`:
the body is a bare `self.insert_upstream(name.to_string(), Arc::new(upstream))`
(DashMap insert returning the old value), circuit-breaker/health reset, then
either a 30s-grace `tokio::spawn` retirement of the old upstream (if other
`Arc` holders exist) or an immediate awaited retirement. There is no
"server map write lock" to piggy-back validation onto.

**Reload serializes on `Engine::reload_lock`, and swaps the config LAST** —
`plug-core/src/engine.rs:133` declares `reload_lock: Mutex<()>` (tokio);
`reload_config` (`:658-664`) takes it for the whole reload:

```rust
pub async fn reload_config(self: &Arc<Self>, new_config: Config)
    -> Result<crate::reload::ReloadReport, anyhow::Error> {
    let _guard = self.reload_lock.lock().await;
    crate::reload::apply_reload(self, new_config).await
}
```

`apply_reload` (`plug-core/src/reload.rs:230`) diffs old vs new config,
performs the stop/start/restart work, and only THEN stores the new config
(step-4 comment in that file: "Swap config atomically before spawning
background tasks…", `engine.store_config(new_config)`). Consequence: while a
reload is mid-flight, the ArcSwap still holds the OLD config — so a
commit-time check that only compares against `self.config.load()` WITHOUT
holding `reload_lock` cannot detect an in-flight reload. Holding
`reload_lock` at commit time can: reload is then either entirely before the
commit (config not yet touched; the reload's own diff will handle whatever
we installed) or entirely after it (config already swapped; we see the new
truth). No torn middle.

**Nothing that holds `reload_lock` calls the installing paths** (verified at
planning time; re-verify at execution):
`grep -n 'restart_server\|reconnect_server\|do_reconnect' plug-core/src/reload.rs`
→ no hits. `reload_lock` is taken ONLY in `reload_config` (`:662`).
`set_server_enabled` (`:633`) calls `reload_config` but is never itself
called under the lock. This is what makes the design below deadlock-free
with a non-reentrant tokio `Mutex`.

**Reload already owns the definition of "materially changed"** —
`plug-core/src/reload.rs:203`, `fn server_config_changed(old, new) -> bool`
(currently private), compares exactly: `command`, `args`, `env`,
`transport`, `url`, `timeout_secs`, `call_timeout_secs`, `enabled`,
`auth_token` (via `.as_str()`), `auth`, `oauth_client_id`, `oauth_scopes`,
`health_check_interval_secs`. `diff_configs` (`:80`) classifies a server as
`changed` (→ reload restarts it) or `unchanged` (→ reload does NOT touch its
process) using this predicate. Fields it deliberately OMITS —
`max_concurrent`, `circuit_breaker_enabled`, `enrichment`, `tool_renames`,
`tool_groups`, `sandbox` — do not trigger a restart on reload.

**Whole-struct equality is not available and would be wrong anyway**:
`ServerConfig` derives only `Debug, Clone, Serialize, Deserialize`
(`plug-core/src/config/mod.rs:371`), and its `auth_token` field is
`SecretString` (`plug-core/src/types.rs:19`), which implements no
`PartialEq` — a derive would not compile without touching the secret type.
Comparing via serde values is forbidden outright: `SecretString`'s
`Serialize` is `#[serde(transparent)]` and deliberately emits plaintext,
with a doc comment (`types.rs:10-17`) warning never to serialize configs
outside persistence. More importantly, equality-vs-predicate is a semantic
question, not a mechanical one — see the comparison contract below.

**Discard/teardown helper exists** — `plug-core/src/server/mod.rs:1781`:
`async fn retire_upstream_owned(name: String, upstream_arc: Arc<UpstreamServer>, reason: &str)`
(currently private, module-level free fn): cancels the client's cancellation
token, then `close_with_timeout(UPSTREAM_REPLACEMENT_SHUTDOWN_TIMEOUT)` with
logging. This is the same routine `replace_server`'s grace path uses.

## Fix design

One new engine-level commit protocol, used by BOTH installing callers:
**connect outside any lock; commit under `reload_lock` after re-validating
the config.**

New method on `Engine` (engine.rs):

```rust
pub(crate) enum ReplaceOutcome {
    /// Config still matches the snapshot — upstream installed.
    Committed,
    /// Server removed or reconfigured concurrently — new upstream retired, map untouched.
    StaleDiscarded,
}

/// Commit a freshly-connected upstream, unless the server's config changed
/// (or the server vanished) since `connected_with` was snapshotted.
/// Holds `reload_lock` across validate+install so an in-flight reload is
/// either fully before or fully after the commit. MUST NOT be called from
/// any path that already holds `reload_lock` (tokio Mutex is not reentrant).
async fn commit_replacement(
    &self,
    server_id: &str,
    connected_with: &ServerConfig,
    upstream: UpstreamServer,   // match start_server's actual return type
) -> ReplaceOutcome {
    let _guard = self.reload_lock.lock().await;
    let current = self.config.load();
    match current.servers.get(server_id) {
        // Same materiality predicate reload uses — see comparison contract.
        Some(cfg) if !server_config_changed(connected_with, cfg) => {
            self.server_manager.replace_server(server_id, upstream).await;
            ReplaceOutcome::Committed
        }
        _ => {
            // Never inserted: we hold the only handle — retire inline, no grace.
            retire_upstream_owned(
                server_id.to_string(),
                Arc::new(upstream),
                "discarded: server removed or reconfigured during reconnect/restart",
            ).await;
            ReplaceOutcome::StaleDiscarded
        }
    }
}
```

Caller changes:

- **`do_reconnect`**: replace the direct `replace_server` call with
  `commit_replacement(server_id, &server_config, upstream)`. On `Committed`,
  keep the existing post-steps exactly (`refresh_tools`, `ServerStarted`
  event, info log). On `StaleDiscarded`, skip ALL post-steps, log
  `tracing::info!(server = %server_id, "reconnect abandoned: server removed or reconfigured during retry")`,
  and return `Ok(())` — the reload already established the desired reality.
- **`restart_server`**: same substitution. On `Committed`, keep the existing
  post-steps (`sync_refresh_loop_for_server`, `refresh_tools`,
  `ServerStarted`, info log). On `StaleDiscarded`, return
  `Err(anyhow!("server '{server_id}' was removed or reconfigured by a concurrent config reload; restart abandoned"))` —
  this is a user-facing command and should say why it didn't do what was
  asked. (The `ServerStopped` event it emitted earlier is accurate either
  way — the old instance WAS stopped.)
- **Cheap early-exit between retries** (optimization, not the correctness
  guarantee): at the top of each retry iteration in `do_reconnect`'s loop,
  re-`load()` the config WITHOUT the lock; if the server is gone or
  `server_config_changed(&snapshot, current_cfg)` — the SAME predicate as
  the commit check, never equality — abandon with the same info log and
  `Ok(())`. A non-material change (e.g. `max_concurrent`) must NOT
  terminate the retry loop: reload started no replacement for it, so
  abandoning here would strand the server down before commit ever ran.
  This avoids tens of seconds of doomed dials.

**Comparison contract (load-bearing — do not substitute equality):** the
commit check MUST use the SAME materiality predicate as reload
(`server_config_changed`), because the discard arm's justification —
"reload already established the desired reality" — is only true for changes
reload acts on. Concrete failure under whole-struct equality: reconnect
snapshots config A; a reload changes only `max_concurrent` (predicate says
UNCHANGED, so reload starts no replacement) and stores config B; equality
sees A ≠ B and discards the one successful reconnection, returning Ok —
the unhealthy server silently stays down. With the shared predicate,
`StaleDiscarded` occurs exactly when reload took authoritative action for
that server (removed → stopped it; materially changed / disabled → the
predicate covers `enabled` → reload's changed-handling dealt with it), and
non-material drift commits the reconnected upstream — identical to how
reload treats running servers for those same fields. If the predicate and
this commit check ever diverge, this bug comes back; there must be ONE
shared function.

Supporting changes:

- Make `server_config_changed` `pub(crate)` so engine.rs can call it
  (reload.rs edit is this one visibility keyword ONLY — no behavior change
  to the reload path).
- Make `retire_upstream_owned` `pub(crate)` so engine.rs can call it
  (server/mod.rs edit is this one visibility keyword ONLY).
- `replace_server` itself stays exactly as-is (unconditional; plan 012 owns
  its grace-spawn). The validation lives in the engine, where `reload_lock`
  lives.

Lock-discipline notes for the implementer (put these in the
`commit_replacement` doc comment):

- Work under the lock is bounded: one ArcSwap load, one comparison, and
  either `replace_server` (DashMap insert + spawn, or an awaited retirement
  bounded by `UPSTREAM_REPLACEMENT_SHUTDOWN_TIMEOUT`) or the same bounded
  retirement of the new upstream. No network connects happen under the lock.
- The reverse interleaving is benign: if the commit wins the lock first,
  reload's diff runs immediately after against the just-installed upstream
  and stops/restarts it per the new config — the end state is still the new
  config's.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Targeted tests | `cargo test -p plug-core engine` and `cargo test -p plug-core server` | all pass |
| Full tests | `cargo test --workspace` | all pass |
| Lint | `cargo clippy --workspace --all-targets -- -D warnings` | exit 0 |
| Format | `cargo fmt --check` | exit 0 |

## Scope

**In scope**:
- `plug-core/src/engine.rs` — `commit_replacement` + `ReplaceOutcome`; wire
  into `do_reconnect` (plus its retry-loop early-exit) and `restart_server`.
- `plug-core/src/server/mod.rs` — `pub(crate)` on `retire_upstream_owned`,
  nothing else.
- `plug-core/src/reload.rs` — `pub(crate)` on `server_config_changed`,
  nothing else.
- Tests for the commit protocol and both callers.

**Out of scope** (do NOT touch):
- `replace_server`'s body, including the 30s grace `tokio::spawn` — plan 012
  fixes its task tracking; keep its behavior byte-for-byte.
- The reload path's BEHAVIOR (`reload.rs`, `reload_config`) — no changes to
  how reload stops/starts servers, when it swaps the config, or what
  `server_config_changed` compares (the only reload.rs edit is the
  visibility keyword). If you believe the predicate should cover more
  fields, report it — do not widen it in this plan.
- `plug-core/src/config/mod.rs` — no derives, no changes (v2 planned a
  `PartialEq` derive here; v2.1 removed the need).
- Health-monitor scheduling/backoff policy (`RECONNECT_RETRY_*` constants,
  attempt counts) — unchanged.
- The `reconnecting` `AtomicBool` claim in `reconnect_server` — reviewed at
  planning time and correct; leave as-is.
- `restart_server`'s rate limiting — unchanged.

## Git workflow

- Branch: `fix/reconnect-reload-interlock`
- Commit: `fix(engine): commit reconnects/restarts under reload_lock so stale upstreams are discarded`.
- Do NOT push or open a PR unless the operator instructed it.

## Steps

### Step 1: Confirm the moving parts

Read `engine.rs:470-670` (restart_server, reconnect_server, do_reconnect,
set_server_enabled, reload_config), `server/mod.rs:1740-1830`
(replace_server, retire_upstream_owned), and `reload.rs:230-330`
(apply_reload — confirm the config store still happens after stop/start).
Re-run the two planning-time greps and confirm both still hold:

- `grep -rn 'replace_server' plug-core/src plug/src` → callers are exactly
  `engine.rs` (restart_server, do_reconnect) + the definition.
- `grep -n 'restart_server\|reconnect_server\|do_reconnect' plug-core/src/reload.rs`
  → no hits (deadlock-freedom precondition).

Confirm `start_server`'s return type (what `commit_replacement` takes by
value), and read `server_config_changed` (`reload.rs:203`) against
`ServerConfig`'s field list: note (report-only, do NOT act on it) any field
the predicate omits that `start_server` bakes into the constructed upstream
(e.g. if `max_concurrent` sizes a semaphore at construction) — that
asymmetry predates this plan and applies equally to reload's own
unchanged-server handling.

**Verify**: both greps match the plan; predicate located and its field list
recorded; you can state why holding `reload_lock` at commit time excludes a
torn reload (write the two-interleaving argument into the doc comment now).

### Step 2: (Optional but recommended) demonstrate the bug first

Before changing code, add a test that reproduces the resurrection with the
CURRENT code — pattern-match the existing engine tests
(`plug-core/src/engine.rs:1083-1109` drive `restart_server` against
fixtures; reuse their engine/mock-server setup): configure `foo`, snapshot
its config, `reload_config` with `foo` removed, then simulate the tail of a
reconnect (`start_server` + `replace_server` with the stale snapshot's
server) and assert the map CONTAINS `foo` again — the bug. Keep the test;
step 3 will flip its assertion (map must NOT contain `foo`).

**Verify**: the bug-demonstration assertion passes against unmodified code
(i.e., resurrection really happens). If the fixtures cannot drive this
(e.g., `start_server` needs a live process the harness can't fake), skip
with a note — do not build new harness machinery for it.

### Step 3: Implement `commit_replacement` + supporting changes

Per the design: the enum, the method with `reload_lock` +
`server_config_changed` + install-or-retire, the two `pub(crate)`
visibility keywords. Then wire both callers, including their divergent
`StaleDiscarded` handling (do_reconnect → `Ok` + info log; restart_server →
`Err`) and the untouched `Committed` post-steps.

**Verify**: `cargo check --workspace` → exit 0.

### Step 4: Implement the retry-loop early-exit

Top of `do_reconnect`'s loop: lock-free config re-load; abandon ONLY on
gone or `server_config_changed` (same predicate as commit — no equality) →
info log + `Ok(())`.

**Verify**: `cargo check --workspace` → exit 0.

### Step 5: Tests

Alongside the existing engine tests (same fixtures):

1. `commit_installs_when_config_unchanged` — snapshot, no reload, commit →
   `Committed`; map serves the new upstream.
2. `commit_discards_when_server_removed` — snapshot, `reload_config` with
   the server deleted, commit → `StaleDiscarded`; map has no entry for it
   (step 2's flipped assertion covers this if step 2 ran).
3. `commit_discards_on_material_change` — snapshot, `reload_config` with
   the server's URL (or command) changed — a field `server_config_changed`
   covers — commit → `StaleDiscarded`; the map still holds the
   RELOAD-installed instance untouched (assert on whatever identity the
   fixtures expose — config value or Arc identity).
4. `commit_installs_on_non_material_change` — snapshot, `reload_config`
   changing ONLY `max_concurrent` (a field the predicate omits, so reload
   starts no replacement), commit → `Committed` and the reconnected
   upstream is installed. This is the regression test for the
   whole-struct-equality bug: under equality it would discard and strand
   the server down.
5. `restart_reports_concurrent_reload` — end-to-end through
   `restart_server` ONLY if a seam exists to interleave a reload between its
   `start_server` and commit (check the fixtures; a mock upstream with a
   test-controlled connect delay is such a seam). If no seam exists, skip
   with a note — tests 1–4 cover the protocol.
6. `reconnect_abandons_between_retries_when_server_removed` — drive
   `do_reconnect` with a mock failing attempt 1; remove the server via
   `reload_config`; assert the loop exits before attempt 2 (dial count == 1).
   Same seam caveat as test 5.
7. `retry_loop_survives_non_material_reload` — mock fails attempt 1
   (retryable); apply a `max_concurrent`-only reload between attempts;
   assert the loop proceeds to attempt 2 (dial count == 2) and, on success,
   the commit installs (`Committed`). Regression for the early-exit using
   equality instead of the predicate. Same seam caveat as tests 5–6.

**Verify**: targeted tests pass; `cargo test --workspace` → all pass;
clippy/fmt gates → exit 0.

## Test plan

Covered by steps 2 and 5. Tests 1–4 are the correctness guarantee (the
commit protocol, including the non-material-change regression); 5–7 are
caller-level and skippable with a note if the harness has no interleaving
seam. If step 2 ran, state in the completion report that the resurrection
was demonstrated pre-fix and prevented post-fix.

## Done criteria

- [ ] `cargo test --workspace` exits 0
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` exits 0
- [ ] `cargo fmt --check` exits 0
- [ ] Tests 1–4 pass (5–7 pass or are documented as not-driveable)
- [ ] Only `engine.rs`, `server/mod.rs` (one visibility keyword), and
      `reload.rs` (one visibility keyword) modified (`git status`);
      `config/mod.rs` untouched
- [ ] The commit check calls `server_config_changed` — no new equality or
      comparison logic exists (grep `PartialEq\|to_value` in the diff → no
      hits)
- [ ] `replace_server`'s body untouched (`git diff` shows no edits between
      its `fn` line and closing brace)
- [ ] Both callers handle `StaleDiscarded` per the design (grep for the two
      new log/error strings)
- [ ] `plans/README-claude-fable.md` status row updated

## STOP conditions

Stop and report back (do not improvise) if:

- The step-1 greps disagree with the plan — `replace_server` grew another
  caller, or anything reachable while holding `reload_lock` calls
  `restart_server`/`reconnect_server`/`do_reconnect` (deadlock risk with a
  non-reentrant tokio Mutex). Report the call chain.
- `server_config_changed` no longer exists by that name, or `diff_configs`
  no longer uses it to decide restart-vs-skip — the shared-predicate
  contract has no anchor; report before inventing a comparison.
- Reading `apply_reload` shows `changed` servers are NOT actually
  stopped/restarted (the discard arm's "reload established reality"
  justification would be false) — report with the code path.
- `retire_upstream_owned` cannot be reused for a never-inserted upstream
  (e.g. it asserts map state you don't have) — report rather than
  duplicating the grace/teardown logic inline.
- Holding `reload_lock` across `replace_server` turns out to await something
  unbounded (it shouldn't — the immediate-retire branch is bounded by
  `UPSTREAM_REPLACEMENT_SHUTDOWN_TIMEOUT`) — report with the await chain.

## Maintenance notes

- Any future code path that installs into the live server map outside reload
  must go through `commit_replacement` (and therefore inherits the staleness
  check) — say this in `ReplaceOutcome`'s doc comment.
- Plan 012 wraps `replace_server`'s grace-spawn in tracked handles; land 011
  first (both touch the same vicinity; 012's plan carries the coordination
  note too).
- **Materiality has ONE definition**: commit and reload share
  `server_config_changed`. Any future config field that affects connection
  identity must be added to that predicate (where reload's restart behavior
  also needs it) — the commit check then inherits it automatically. Never
  reintroduce a second comparison (equality, serde values, field subsets)
  on the commit side.
- Review focus: no `.await` under `reload_lock` other than
  `replace_server`/retirement (both bounded); the two `StaleDiscarded`
  caller behaviors (Ok-and-log vs Err) stay divergent on purpose; no
  plaintext-secret serialization anywhere in the diff.
