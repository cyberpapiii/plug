# Plan 020: Bound the downstream-OAuth token stores and pin the unenforced-scope behavior

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README-claude-fable.md` — unless a reviewer dispatched you and
> told you they maintain the index.
>
> **Drift check (run first)**:
> `git diff --stat e341625..HEAD -- plug-core/src/downstream_oauth/mod.rs`
> If the file changed since this plan was written, compare the "Current
> state" excerpts against the live code before proceeding; on a mismatch,
> treat it as a STOP condition.

## Status

- **Priority**: P2
- **Effort**: M
- **Risk**: MED (touches an auth path; mitigated by the module's existing
  in-file test suite and by keeping issuance/validation semantics unchanged
  except where stated)
- **Depends on**: none. Related but independent: plan 018 (downstream OAuth
  conformance SPIKE, docs-only) — see Maintenance notes for what this plan
  hands to it.
- **Category**: security
- **Planned at**: commit `e341625`, 2026-07-12

## Why this matters

When `http.auth_mode = "oauth"`, plug is a real OAuth 2.1 authorization
server for downstream HTTP clients: RFC 8414/9728 metadata, PKCE-S256
authorize, a token endpoint with authorization_code / refresh_token /
client_credentials grants, and bearer validation on `/mcp`. Two hardening
gaps are live:

1. **Unbounded-ish stores**: eviction is lazy-only — an expired entry is
   removed only if that specific entry is later re-presented. Issued-and-
   never-presented access tokens, abandoned auth codes, and expired refresh
   tokens linger in memory AND in the persisted state file forever. Worst
   case is the `client_credentials` grant: it mints a fresh access token on
   EVERY call, so a well-behaved-but-stateless client that requests a token
   per call grows `access_tokens` monotonically for the process lifetime —
   and every mutation re-serializes the WHOLE state to disk, so the
   per-issuance cost grows with the garbage.
2. **Scopes are issued but never enforced**: `validate_access_token`
   returns a bare bool and explicitly discards the record's scopes; the MCP
   middleware only distinguishes authenticated/unauthorized. Any valid
   token grants full access to every merged tool/resource/prompt.

This plan fixes (1) mechanically (eager sweep + client_credentials token
reuse) and makes (2) explicit and pinned by a characterization test —
deliberately NOT inventing scope semantics here, because "what should a
scope mean in an MCP multiplexer" is a design question that belongs to plan
018's conformance spike (todo 057, `ready`/p1, already flags downstream
OAuth standards gaps).

One false alarm to put to rest (so nobody re-audits it): **persistence is
fine.** Tokens survive restart via an atomic temp+rename 0600 JSON file
under the config dir; auth codes are intentionally non-persistent; there is
no dynamic client registration (client identity is the single static
`oauth_client_id`/`oauth_client_secret` pair from config), so there is no
client-registration state to persist.

## Current state

All excerpts verified at the planned-at commit. Everything below is
`plug-core/src/downstream_oauth/mod.rs` unless stated.

- Lifetimes (`:17-19`):

  ```rust
  const AUTH_CODE_LIFETIME_SECS: u64 = 300;
  const ACCESS_TOKEN_LIFETIME_SECS: u64 = 3600;
  const REFRESH_TOKEN_LIFETIME_SECS: u64 = 30 * 24 * 3600;
  ```

- The state (`:71-79`) — three maps, no clients map:

  ```rust
  #[derive(Debug, Default, Clone, Serialize, Deserialize)]
  struct DownstreamOauthState {
      #[serde(default)]
      pending_codes: HashMap<String, PendingAuthorizationCode>,
      #[serde(default)]
      access_tokens: HashMap<String, IssuedAccessToken>,
      #[serde(default)]
      refresh_tokens: HashMap<String, IssuedRefreshToken>,
  }
  ```

- Insert sites, each followed by `persist_state(&self.config, &guard)`:
  auth code `:205`; code exchange mints refresh `:253` + access `:261`;
  refresh exchange mints access `:305` (the PREVIOUS access token minted
  for that refresh token is not removed — it stays valid until expiry,
  which is RFC-acceptable and, after this plan, bounded by the sweep);
  client_credentials mints access `:346-354` with
  `refresh_token: String::new()` as the no-refresh marker.
- The client_credentials grant (`:324-363`) — the unbounded case:
  every call does `uuid → insert → persist`; nothing is ever reused or
  replaced.
- Lazy-only eviction: expired access token removed only when that token is
  presented (`:374-378` inside `validate_access_token`); expired refresh
  token only when presented (`:298-302`); expired auth code only when
  presented (`:238-241`). No sweep, no size cap anywhere in the module.
- Scope discard (`:365-381`):

  ```rust
  pub async fn validate_access_token(&self, token: &str) -> bool {
      let mut guard = self.state.lock().await;
      match guard.access_tokens.get(token) {
          Some(record) if record.expires_at >= epoch_secs() => {
              let _ = &record.client_id;
              let _ = &record.refresh_token;
              let _ = &record.scopes;
              true
          }
  ```

- Persistence: `load_persisted_state` `:484` (called from `new`),
  `persist_state` `:501` — temp file with `0o600` (`:531`, `:541`) then
  `std::fs::rename` (`:551`). Auth codes are cleared on both load and
  persist (ephemeral by design).
- Wiring (context; NOT in scope): routes at
  `plug-core/src/http/server.rs:372-373` point `/oauth/authorize` and
  `/oauth/token` at handlers named `oauth_authorize_not_implemented`
  (`:1074`) / `oauth_token_not_implemented` (`:1118`) — **the names are
  stale; the bodies are fully implemented and call the manager.** Do not
  conclude from the names that this is stub code. Metadata builds the
  endpoints at `:1046-1047`. `DownstreamAuthMode` enum (Auto default /
  None / Bearer / Oauth) at `plug-core/src/config/mod.rs:341-349`.
- The module has a real in-file test suite (`#[tokio::test]`s from `:577`,
  including `issued_tokens_survive_manager_recreation` at `:577-623`) —
  model new tests on these.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Build check | `cargo check` | exit 0 |
| Module tests | `cargo test -p plug-core downstream_oauth` | all pass |
| Full tests | `cargo test --workspace` | all pass |
| Lint | `cargo clippy --workspace --all-targets -- -D warnings` | exit 0 (see done-criteria caveat) |
| Format | `cargo fmt --check` | exit 0 |

## Scope

**In scope** (the only file you should modify):
- `plug-core/src/downstream_oauth/mod.rs` — the sweep, the
  client_credentials reuse, and tests in the existing in-file test module.

**Out of scope** (do NOT touch, even though they look related):
- Scope ENFORCEMENT (mapping scopes to tools/servers/operations) — design
  question owned by plan 018's spike; here we only pin current behavior.
- `plug-core/src/http/server.rs` — routes, middleware, metadata, and the
  misleading handler NAMES all stay (renaming them is cosmetic churn that
  would collide with other plans touching that file).
- Upstream OAuth (`plug-core/src/oauth.rs`) — separate mature module; not
  implicated.
- The persistence format/mechanism — atomic temp+rename stays; the O(n)
  full-file write per mutation is acceptable once n is bounded (note kept
  in Maintenance).
- Dynamic client registration — deliberately absent; do not add.

## Git workflow

- Branch: `advisor/020-downstream-oauth-store-hardening` off `main`.
- Conventional commits; suggested:
  `fix(auth): sweep expired downstream-oauth entries and reuse client_credentials tokens`.
- Do NOT push or open a PR unless the operator instructed it.

## Steps

### Step 1: Read the module end to end

Read `plug-core/src/downstream_oauth/mod.rs` fully (858 lines). Confirm the
excerpts above, the exact struct field sets of `IssuedAccessToken` /
`IssuedRefreshToken` / `PendingAuthorizationCode`, and that `epoch_secs()`
is the module's single time source.

**Verify**: excerpts match; note in your log the exact line numbers you
found (they will have drifted slightly if other plans landed).

### Step 2: Add an eager expiry sweep

Add to `DownstreamOauthState`:

```rust
fn evict_expired(&mut self, now: u64) {
    self.pending_codes.retain(|_, c| c.expires_at >= now);
    self.access_tokens.retain(|_, t| t.expires_at >= now);
    self.refresh_tokens.retain(|_, t| t.expires_at >= now);
}
```

Call `guard.evict_expired(epoch_secs());` at the top of EVERY method that
takes the state lock to mutate — `start_authorization` (the pending-code
insert path), `exchange_authorization_code`, `exchange_refresh_token`,
`exchange_client_credentials` — immediately after acquiring the guard,
before any lookup. Do NOT add it to `validate_access_token` (the hot
per-request path keeps its existing presented-token-only eviction; sweeps
ride the rare mutation paths) and do NOT sweep inside
`load_persisted_state` (restart must not silently drop entries a test — or
an operator inspecting the file — expects to see; the first mutation will
sweep).

Note: the sweep runs BEFORE lookups, which changes one observable detail —
presenting an expired code/refresh token after a sweep has run yields
`InvalidGrant` (entry gone) rather than `TokenExpired`. Both are 4xx token
errors; if an existing test asserts `TokenExpired` for an entry that a
sweep would have removed, place the sweep AFTER the method's
expired-on-presentation check instead, and note it. Semantics, not error
flavor, are the contract.

**Verify**: `cargo test -p plug-core downstream_oauth` → all existing tests
pass (adjusting only error-variant expectations if the note above applied —
record any such adjustment).

### Step 3: Reuse client_credentials tokens

In `exchange_client_credentials` (`:324-363`), before minting: search
`guard.access_tokens` for an entry with `client_id` matching,
`refresh_token.is_empty()` (the CC marker), `scopes` equal to the resolved
requested scopes, and `expires_at >= epoch_secs() + 60` (don't hand out a
token about to die; the 60s floor forces a fresh mint near expiry). If
found, return THAT token with `expires_in = expires_at - epoch_secs()` (the
REMAINING lifetime, not a fresh 3600) and skip the insert+persist entirely.
Otherwise mint exactly as today.

This bounds the CC store at one live token per distinct scope set (plus at
most one near-expiry predecessor until the next sweep) instead of one per
call, and it never invalidates a token another process still holds.

**Verify**: `cargo test -p plug-core downstream_oauth` → passes, including
the new reuse test from the test plan.

## Test plan

Add to the module's existing in-file test module, modeled on the tests at
`:577+`:

1. `evict_expired_drops_only_expired_entries` — build a state with one
   live + one expired entry in each of the three maps (construct
   `expires_at` values directly; `evict_expired` takes `now` as a
   parameter, so no clock control is needed); assert exactly the three
   expired entries are gone.
2. `abandoned_auth_code_swept_on_next_mutation` — start an authorization
   (code inserted), advance `now` past `AUTH_CODE_LIFETIME_SECS` by
   constructing the state/manager accordingly or by calling
   `evict_expired` with a future `now`; perform an unrelated mutation
   (e.g. a client_credentials exchange); assert the stale code is gone
   from the state AND from the persisted file (re-load and check).
3. `client_credentials_reuses_live_token` — two identical CC exchanges:
   assert the same `access_token` string comes back, the second response's
   `expires_in` ≤ the first's, and the access-token map has exactly ONE
   CC entry for that scope set.
4. `client_credentials_mints_new_token_for_different_scopes` — CC with
   scope "a" then scope "b": two distinct tokens, both valid.
5. `CHARACTERIZATION: any_valid_token_grants_full_access` — issue a token
   with a narrow scope, assert `validate_access_token` returns plain
   `true` (document in the test comment: scopes are issued and persisted
   but NOT enforced; enforcement semantics are deferred to the plan-018
   spike — this test pins today's behavior so a future enforcement change
   is a deliberate, visible test edit).
6. Persistence round-trip guard: extend or copy
   `issued_tokens_survive_manager_recreation` to assert a live CC-reused
   token still validates after manager recreation.

Negative check: temporarily disable the `evict_expired` calls (comment
out), run test 2 — it must FAIL; restore. Do NOT use `git stash` (shared
worktree).

## Done criteria

Machine-checkable. ALL must hold:

- [ ] `grep -c 'fn evict_expired' plug-core/src/downstream_oauth/mod.rs` → 1,
      and `grep -c 'evict_expired(' plug-core/src/downstream_oauth/mod.rs`
      → ≥5 (definition + four mutation-path call sites)
- [ ] `cargo test -p plug-core downstream_oauth` exits 0 with the 6 new
      tests present and passing
- [ ] `cargo test --workspace` exits 0
- [ ] Negative check demonstrated (recorded in completion notes)
- [ ] `cargo fmt --check` exits 0
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` exits 0.
      **Known pre-existing failure caveat**: at the planned-at commit this
      gate is RED for two findings unrelated to this plan (`question_mark`
      at `plug-core/src/artifacts.rs:482`, `for_kv_map` at
      `plug-core/src/server/mod.rs:774` — plan 001 step 0 fixes them). If
      clippy fails with EXACTLY those two, record it and treat this
      criterion as met.
- [ ] `git status` shows only `plug-core/src/downstream_oauth/mod.rs`
      modified
- [ ] `plans/README-claude-fable.md` status row updated

## STOP conditions

Stop and report back (do not improvise) if:

- The module structure no longer matches the excerpts (drift).
- An existing test fails for a reason OTHER than the
  `TokenExpired`-vs-`InvalidGrant` error-variant note in step 2 — report;
  do not weaken auth tests to pass.
- The CC-reuse comparison needs scope ORDER normalization (i.e. callers
  send the same scopes in different orders and the reuse never hits) —
  implement a sorted comparison ONLY if the existing code already treats
  scopes as order-insensitive elsewhere; otherwise report the ambiguity.
- You are tempted to enforce scopes "while you're in there" — don't; that
  is plan 018's design output.

## Maintenance notes

- **Handed to plan 018's spike** (record this in that plan's execution):
  the scope-semantics question — what a downstream scope should grant in a
  multiplexer (per-server? per-tool-group? read-only?) — plus the
  stale-handler-name cosmetic cleanup (`oauth_*_not_implemented`) and
  whether metadata should stop advertising scopes it does not enforce.
- The O(n) full-state rewrite per mutation is acceptable once n is bounded
  by the sweep; if downstream OAuth ever serves many clients, revisit with
  an append-or-partition persistence scheme.
- Reviewer should scrutinize: `validate_access_token`'s hot path gained no
  sweep (per design), and `load_persisted_state` still loads everything
  (no silent restart-time drops).
- Future DCR (dynamic client registration), if ever added, introduces a
  fourth store — it must join `evict_expired` and the persistence file
  from day one.
