# Plan 023: Fetch the three live catalog families concurrently in `refresh_tools`

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README-claude-fable.md` — unless a reviewer dispatched you and
> told you they maintain the index.
>
> **Drift check (run first)**:
> `git diff --stat e341625..HEAD -- plug-core/src/proxy/mod.rs plug-test-harness/src/bin/mock-server.rs plug-core/tests/integration_tests.rs`
> If any in-scope file changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding; on a
> mismatch, treat it as a STOP condition.

## Status

- **Priority**: P3
- **Effort**: S/M (S for the fix, M with the latency test)
- **Risk**: LOW-MED (concurrency change on the catalog path; behavior
  otherwise identical)
- **Depends on**: none — **but plans 004 and 010 also edit
  `proxy/mod.rs`'s `refresh_tools`; never run 004, 010, and 023 in
  parallel. Any sequential order works; if 004 or 010 landed first, expect
  drift in this plan's line numbers and re-anchor by function name.**
- **Category**: perf
- **Planned at**: commit `e341625`, 2026-07-12

## Why this matters

`ToolRouter::refresh_tools` opens with four back-to-back awaits: tools,
resources, resource templates, prompts. The tools call is ~free (it reads a
per-upstream in-memory cache). The other three each do live upstream I/O —
and each is ALREADY concurrent across servers internally (`join_all` with a
per-server `call_timeout_secs` bound). So a refresh's upstream latency is
the SUM of three family-level maxima, when it could be the MAX of the three:
with one slow-but-connected upstream near its call timeout, a refresh stalls
up to 3× longer than necessary. Refresh runs at startup, on (debounced)
list_changed notifications, on health recovery, and on config reload —
startup and reload latency improve directly. This is a small, honest win:
one `tokio::join!` at the head of the function; the per-server concurrency
inside each family getter already exists and is not touched.

An earlier audit claim said "servers are iterated serially" — that is FALSE
for the three live families (verified: `join_all` inside each getter) and
true only for the cache-only tools loop, where it costs nothing. Only the
family-level serialization is real.

## Current state

- `plug-core/src/proxy/mod.rs:1081-1099` — the head of `refresh_tools`:

  ```rust
  pub async fn refresh_tools(&self) {
      let upstream_tools = self.server_manager.get_tools().await;
      let resources_result = self.server_manager.get_resources().await;
      let resource_templates_result = self.server_manager.get_resource_templates().await;
      let prompts_result = self.server_manager.get_prompts().await;

      // Recompute per-server availability from this cycle's listing outcomes.
      // A server served from last-known-good cache (its live listing was
      // unavailable) is degraded; its carried-forward entries below keep its
      // URI set unchanged, so the subscription prune logic leaves it untouched.
      let mut degraded_servers = std::collections::BTreeSet::new();
      degraded_servers.extend(resources_result.degraded.iter().cloned());
      degraded_servers.extend(resource_templates_result.degraded.iter().cloned());
      degraded_servers.extend(prompts_result.degraded.iter().cloned());
      self.server_manager.update_availability(&degraded_servers);

      let upstream_resources = resources_result.items;
      let upstream_resource_templates = resource_templates_result.items;
      let upstream_prompts = prompts_result.items;
  ```

  Everything after this is CPU-bound snapshot building (classify, prefix,
  sort, ArcSwap store) — no further upstream awaits.
- `plug-core/src/server/mod.rs:1278-1295` — `get_tools`: a serial loop over
  servers doing NO network I/O (`upstream.tools.load()` reads an `ArcSwap`
  cache filled at connect and refreshed out-of-band on tools/list_changed).
  Zero refresh-latency contribution; not a target.
- `plug-core/src/server/mod.rs:1297+` — `get_resources` (and, same-shaped,
  `get_resource_templates` ~:1375, `get_prompts` ~:1447): filter routable
  servers with the capability, sort, then:

  ```rust
  let results = join_all(
      targets
          .into_iter()
          .map(|(server_name, upstream)| async move {
              let timeout = Duration::from_secs(upstream.config.call_timeout_secs);
              let outcome = match tokio::time::timeout(
                  timeout,
                  upstream.client.peer().list_all_resources(),
              )
              .await
  ```

  — already concurrent across servers, per-server timeout-bounded, with
  fresh results written to a per-family last-known-good DashMap
  (`last_resources` at :1352; templates and prompts have their own maps).
- Facts that make the join safe (confirm on read):
  - The three getters take `&self` and return owned `ListingResult`s —
    no `&mut`, no lock held across the awaits at the call site.
  - The degraded-set union (:1091-1095) consumes all three results AFTER
    they complete; `tokio::join!` preserves that.
  - Each family writes a DISTINCT last-known-good map — no cross-family
    write contention.
  - Post-fetch processing has no ordering dependency between families.
  - Pagination (`list_all_*` looping `next_cursor` inside rmcp) is
    inherently serial per server per family — page N+1 needs page N's
    cursor. NOT a target.
- One NEW behavior this introduces: a single upstream now receives its
  resources/templates/prompts list requests simultaneously instead of
  sequentially. rmcp multiplexes requests by id over the shared transport,
  so this is protocol-legal; a single-threaded upstream may serialize them
  internally (correctness unaffected; the latency win shrinks for that
  server only).
- Pre-existing and OUT of scope: `refresh_tools` has no single-flight guard
  (the notification path debounces via atomics, but health/startup/reload/
  manual callers can overlap it; the tail is last-writer-wins on an
  ArcSwap). Unchanged by this plan.
- Test infrastructure: the mock upstream is
  `plug-test-harness/src/bin/mock-server.rs`, already flag-gated for
  prompts/completion/resource-template handlers;
  `plug_test_harness::mock_server_bin()` (plug-test-harness/src/lib.rs:29)
  builds it once and returns the path. Integration tests live in
  `plug-core/tests/integration_tests.rs` (see ~:1676-1730 for the pattern
  that spawns the mock via a config's command).

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Build check | `cargo check` | exit 0 |
| Tests | `cargo test --workspace` | all pass |
| Just the new test | `cargo test -p plug-core --test integration_tests catalog_families` | pass |
| Lint | `cargo clippy --workspace --all-targets -- -D warnings` | exit 0 (see done-criteria caveat) |
| Format | `cargo fmt --check` | exit 0 |

## Scope

**In scope** (the only files you should modify):
- `plug-core/src/proxy/mod.rs` — ONLY the `refresh_tools` head
  (:1082-1085): replace the three serial live-family awaits with one
  `tokio::join!`.
- `plug-test-harness/src/bin/mock-server.rs` — add an optional
  list-response delay flag (test-only capability, default off).
- `plug-core/tests/integration_tests.rs` — the new latency test.

**Out of scope** (do NOT touch, even though they look related):
- `ServerManager::get_resources/get_resource_templates/get_prompts` —
  already server-concurrent; no change.
- `get_tools` and the upstream tools cache — cache-only; no change.
- rmcp pagination loops — inherently serial; not fixable here.
- A single-flight guard for `refresh_tools` — pre-existing separate
  concern; do not add one "while you're in there".
- The prune/rebind block later in `refresh_tools` (plan 010's territory)
  and the loop-hoist targets (plan 004's territory).

## Git workflow

- Branch: `advisor/023-catalog-family-concurrent-fetch` off `main`.
- Conventional commits (repo style: `fix(ci): …`, `docs(snapshot): …`);
  suggested: `perf(catalog): fetch resource/template/prompt families concurrently`.
- Do NOT push or open a PR unless the operator instructed it.

## Steps

### Step 1: Re-read the head and confirm the join is safe

Read `refresh_tools` (proxy/mod.rs:1076-1100) and the three getters'
signatures in server/mod.rs. Confirm: `&self` receivers, owned results, no
guard held at the call site, and that `futures::future::join_all` is already
imported in server/mod.rs (so the pattern is established; for proxy/mod.rs
you will use `tokio::join!`, which needs no import).

**Verify**: the excerpts in "Current state" match the live code.

### Step 2: Make the three live fetches concurrent

Replace lines 1083-1085 with a single join, keeping the cache-only tools
call as-is above it:

```rust
let upstream_tools = self.server_manager.get_tools().await;
let (resources_result, resource_templates_result, prompts_result) = tokio::join!(
    self.server_manager.get_resources(),
    self.server_manager.get_resource_templates(),
    self.server_manager.get_prompts(),
);
```

Leave the availability comment block and the degraded-union code
(:1087-1095) byte-identical. Add one line to the function's doc comment
noting the three live families fetch concurrently (each already
server-concurrent internally).

**Verify**: `cargo check` → exit 0; `cargo test --workspace` → all pass
(the existing suite exercises `refresh_tools` heavily; identical results
are expected because only completion ORDER changed, not content).

### Step 3: Give the mock server an optional list delay

In `plug-test-harness/src/bin/mock-server.rs`, add a flag (match the
existing flag-gating style used for the prompts/completion/template
handlers — read how those flags are parsed and gate behavior) named
`--list-delay-ms <n>`: when set, the resources/templates/prompts list
handlers sleep that long before responding. Default: no delay (all existing
users of the mock are unaffected).

**Verify**: `cargo test --workspace` still passes (proves the default-off
flag changed nothing — the parity matrix and existing integration tests are
the guard).

### Step 4: Add the latency test

In `plug-core/tests/integration_tests.rs`, add
`catalog_families_fetch_concurrently`, modeled structurally on the existing
mock-spawning test at ~:1676-1730:

- One upstream server configured to run the mock with
  `--list-delay-ms 600` (plus whatever flags enable resources, templates,
  and prompts so all three families actually fetch).
- Build the engine, let it connect, then measure ONE explicit
  `refresh_tools` (or engine-level refresh) with `std::time::Instant`.
- Assert elapsed `< Duration::from_millis(1500)`. Serial fetching would
  take ≥ 1800ms (3 × 600ms); concurrent takes ~600ms plus overhead. The
  700-900ms margin is deliberate anti-flake headroom (plan 014's
  philosophy: generous bounds, poll/measure — never tight sleeps).
- Clean up the engine (cancel token) at the end.

**Verify**: `cargo test -p plug-core --test integration_tests catalog_families`
→ passes. Run it 3 times; all pass.

### Step 5: Negative check (proves the test detects the old behavior)

Temporarily revert step 2's join back to the three serial awaits (keep the
test), run the new test once — it must FAIL on the elapsed assertion. Then
restore the join and confirm it passes. Do this by editing the code back
and forth, NOT with `git stash` (this worktree is shared with another
agent; stash can capture their edits).

**Verify**: test fails on serial code, passes on joined code. Record both
elapsed times in the completion notes.

## Test plan

Covered by steps 3-5: one new integration test
(`catalog_families_fetch_concurrently`) asserting wall-clock refresh
latency ~max instead of ~sum of the three delayed families, plus the full
existing suite as the behavior-identity guard.

## Done criteria

Machine-checkable. ALL must hold:

- [ ] `grep -n 'tokio::join!' plug-core/src/proxy/mod.rs` → one match inside
      `refresh_tools`
- [ ] `grep -c 'get_resources().await;' plug-core/src/proxy/mod.rs` → 0
      (the serial form is gone)
- [ ] `cargo test --workspace` exits 0, including
      `catalog_families_fetch_concurrently`
- [ ] Step 5's negative check demonstrated (recorded in completion notes)
- [ ] `cargo fmt --check` exits 0
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` exits 0.
      **Known pre-existing failure caveat**: at the planned-at commit this
      gate is RED for two findings unrelated to this plan (`question_mark`
      at `plug-core/src/artifacts.rs:482`, `for_kv_map` at
      `plug-core/src/server/mod.rs:774` — plan 001 step 0 fixes them). If
      clippy fails with EXACTLY those two, record it and treat this
      criterion as met.
- [ ] `git status` shows only the three in-scope files modified
- [ ] `plans/README-claude-fable.md` status row updated

## STOP conditions

Stop and report back (do not improvise) if:

- The `refresh_tools` head no longer matches the excerpt (plan 004 or 010
  landed first) — re-anchor by function name, and if the four-await shape is
  gone entirely, report instead of re-deriving the change.
- Any EXISTING test fails after step 2 — the join is supposed to be
  result-identical; a failure means a real ordering dependency this plan
  says doesn't exist. Report the test name; do not weaken the test.
- The new test flakes (fails ≥1 of 3 runs) at the stated margins — report
  measured timings rather than tightening or loosening bounds ad hoc.
- Adding the mock delay flag requires restructuring mock-server.rs's
  existing handlers — report; the flag must be additive.

## Maintenance notes

- If a future change adds a FOURTH live family (or makes tools listing
  live), it belongs inside the same `tokio::join!`.
- Reviewer should scrutinize: the degraded-union block is untouched, and
  the mock's delay flag defaults off.
- Deliberately deferred: single-flight/coalescing for overlapping
  `refresh_tools` calls (pre-existing last-writer-wins behavior, separate
  finding); per-server fetch fan-out limits (unbounded `join_all` breadth is
  pre-existing and fine at personal-config scale).
