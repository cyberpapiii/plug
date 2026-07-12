# Plan 015: Deduplicate the notification fanout logic triplicated across the three transports

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README-claude-fable.md`.
>
> **Drift check (run first)**: `git diff --stat e341625..HEAD -- plug-core/src/proxy/handler.rs plug-core/src/http/server.rs plug/src/daemon.rs`
> If any of the three fanout sites changed since this plan was written,
> re-derive the classification logic from the LIVE code (the three sites must
> be re-compared line by line) before extracting anything. Another AI agent
> (Codex) may be working in this repo concurrently.

## Status

- **Priority**: P2 (highest-leverage debt item — every notification feature pays the 3× tax)
- **Effort**: M
- **Risk**: MEDIUM (behavior-preserving refactor of a hot path; parity matrix is the net)
- **Depends on**: none. **Blocks plan 016** (daemon.rs decomposition moves the
  daemon fanout — extract shared logic FIRST so 016 moves one thin site, not
  the triplicated blob).
- **Category**: tech debt
- **Planned at**: commit `e341625`, 2026-07-11

## Why this matters

Every upstream notification (logging, progress, resources/updated,
list_changed × 3) is classified and routed to downstream clients in THREE
nearly-identical blocks — one per transport:

- stdio: `plug-core/src/proxy/handler.rs:251-345` (progress special-casing ~`:289`)
- HTTP: `plug-core/src/http/server.rs:187-296` (progress ~`:223`)
- daemon IPC: `plug/src/daemon.rs:1321-1420` (progress ~`:1360`)

Each block re-implements: notification-type classification, progress-token
lookup → route-to-requester, subscription check for `resources/updated`,
client-capability gating, and target resolution. The blocks have already
drifted in small ways historically (the cross-transport parity test matrix
exists precisely because of this), and every new notification type or routing
rule costs three implementations plus three review passes. The dispatcher
migration (plan 017's design doc; `plug-core/src/dispatch/mod.rs:15` says "Only
tools/call is migrated here today") will eventually need fanout unified
anyway — this extraction is its first concrete step.

**Shape of the fix (already decided)**: extract the DECISION logic
(classify + resolve targets), keep the DELIVERY (actually sending on a
transport's wire) per-transport. Decision is pure-ish and identical; delivery
is genuinely different (stdio peer writes vs SSE sessions vs IPC frames).

Correction (2026-07-11, verified against `e341625` during cross-agent
review): the IPC identity split has ALREADY LANDED on `main` —
`DownstreamTransport::Ipc` exists (`plug-core/src/proxy/mod.rs:278`) and
`NotificationTarget` has three first-class variants `Stdio`/`Http`/`Ipc`
(`plug-core/src/notifications.rs:51`; IPC "no longer masquerades as Stdio"
per the doc comment). The shared resolver therefore works with all THREE
existing variants as-is; do not add or remove variants in this plan.

## Current state

Verified at commit `e341625` (the three ranges above). At execution time,
read all three blocks side by side FIRST and build a difference table —
every behavioral difference between them is either (a) transport-inherent
(keep, in the per-transport delivery), or (b) accidental drift (a bug in at
least one of them — record it, preserve each site's current behavior in this
plan, and report the drift in the completion notes; fixing drift is a
follow-up decision, not silent).

Shared vocabulary that already exists (grep before creating anything):
- `NotificationTarget` (enum: `Stdio`, `Http`, ... — `grep -rn 'enum NotificationTarget' plug-core/src plug/src`)
- progress-token → requester registry (used by all three progress arms —
  find the lookup call each block makes)
- subscription registry check for `resources/updated` (plan 010 touches the
  registry's subscribe/unsubscribe internals, NOT the read path used here —
  no conflict, but if 010 landed, re-verify the read API name)

## Fix design

New module `plug-core/src/notify/fanout.rs` (or extend an existing
notifications module if one exists — `ls plug-core/src` first; do not create
a parallel home if `notify/` or similar already has the vocabulary):

```rust
/// What a notification is, after classification — transport-independent.
pub enum NotificationClass {
    Progress { token: ProgressToken /* real type */ },
    ResourceUpdated { uri: String },
    ListChanged(ListChangedKind), // tools/resources/prompts
    Logging,
    Other,
}

/// Who should receive it — resolved against registries, still transport-independent.
pub enum ResolvedDelivery {
    /// Route to the single client that owns this progress token.
    ToRequester(ClientKey /* real key type */),
    /// Fan out to clients passing the given filter (subscription/capability).
    Broadcast(BroadcastFilter),
    Drop,
}

pub fn classify(notification: &ServerNotification /* real rmcp type */) -> NotificationClass;

pub fn resolve(
    class: &NotificationClass,
    registries: &FanoutRegistries<'_>, // borrows of the progress + subscription registries
) -> ResolvedDelivery;
```

Each transport's block becomes: `classify` → `resolve` → transport-local
delivery match (its existing send code). The exact signatures WILL need
adaptation to the real types — the non-negotiable boundary is: **no wire
writes, no transport types (axum/SSE/IPC frames/stdio peers) inside
`plug-core/src/notify/fanout.rs`**; `plug-core`'s existing dependency on its
own http module notwithstanding, the new module stays transport-free so
`plug` (stdio + daemon) can call it too.

Note crate locations (corrected 2026-07-11): `handler.rs` and `server.rs`
BOTH live in `plug-core` (`plug-core/src/proxy/handler.rs`,
`plug-core/src/http/server.rs`); only `daemon.rs` lives in the `plug` crate.
The shared module still must live in `plug-core` (plug depends on plug-core,
not vice versa) so the daemon block can call it. Verify the registries the
resolver needs are reachable from plug-core (they are today — the stdio and
http blocks already use them in-crate; the daemon block reaches them through
the engine handle — find how and mirror it).

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Parity matrix | `cargo test --workspace parity` (adjust to the real test-name filter — find it: `grep -rn 'parity' plug-core/tests plug/tests -l`) | all pass |
| Full tests | `cargo test --workspace` | all pass |
| Lint | `cargo clippy --workspace --all-targets -- -D warnings` | exit 0 |
| Format | `cargo fmt --check` | exit 0 |

## Scope

**In scope**:
- New `plug-core/src/notify/fanout.rs` (+ `mod` wiring).
- The three fanout blocks, each reduced to classify/resolve calls + local delivery.
- Unit tests for `classify`/`resolve`.

**Out of scope** (do NOT touch):
- `NotificationTarget` variants — the enum already has its final three-variant shape (`Stdio`/`Http`/`Ipc`, see correction note above); do not add, remove, or rename variants.
- Delivery mechanics (SSE session lookup, IPC frame encoding, stdio peer writes) — stay where they are.
- ANY behavior change, including fixing drift discovered in step 1 — preserve each site's behavior exactly; drift is reported, not fixed.
- The reverse-request paths (elicitation/sampling) — different machinery.
- dispatch/ module — plan 017 designs that.

## Git workflow

- Branch: `refactor/notification-fanout-dedup`
- Commits: `refactor(notify): extract transport-independent fanout classification`, then one commit per transport migration (stdio, http, daemon) — each independently green.
- Do NOT push or open a PR unless the operator instructed it.

## Steps

### Step 1: Difference table

Read all three blocks fully. Build the table: rows = behaviors (each
notification type's classification, progress routing, subscription gating,
capability gating, unknown-type handling, error handling); columns = the
three sites. Mark each cell identical / transport-inherent / DRIFT. Save the
table — it goes in the completion report verbatim.

**Verify**: table covers every notification type each block handles; any
DRIFT cells listed separately.

### Step 2: Extract `classify` + `resolve` with unit tests

Implement from the table's "identical" rows. Where a DRIFT cell exists, the
shared function takes the SUPERSET and each call site keeps its current
behavior via its own parameters/match arms (behavior preservation beats
elegance this round). Unit-test the pure functions directly: one test per
NotificationClass arm, one per ResolvedDelivery decision, including the
progress-token-unknown case (verify what each site does today — likely drop —
and pin the shared default to that).

**Verify**: `cargo test -p plug-core notify` (or the module's filter) → new tests pass; `cargo check --workspace` → exit 0.

### Step 3: Migrate one transport at a time — stdio, then HTTP, then daemon

For each: replace the block's decision logic with classify/resolve, keep the
delivery match, delete the now-dead local code. After EACH migration:

**Verify**: parity matrix passes + full `cargo test --workspace` passes,
BEFORE starting the next transport. Commit per transport.

### Step 4: Final sweep

Confirm the three delivery matches contain no residual classification (grep
each block for notification method-name string literals — after migration
those literals should exist only in `fanout.rs` and tests).

**Verify**: full gates (test/clippy/fmt) green; difference table + any drift
findings written into the completion report.

## Test plan

- New unit tests for classify/resolve (step 2).
- The cross-transport parity matrix is the primary regression net — it exists
  because of exactly this triplication; if it passes after each migration,
  behavior is preserved for covered paths.
- Coverage gap honesty: if the parity matrix does NOT cover some notification
  type (check its cases against your step-1 table), say so in the completion
  report; do not silently rely on it.

## Done criteria

- [ ] `cargo test --workspace` exits 0 (including the parity matrix) after EACH transport migration commit
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` exits 0
- [ ] `cargo fmt --check` exits 0
- [ ] Notification method-name literals appear only in `fanout.rs` + tests (step-4 grep)
- [ ] Zero behavior change: any drift found is REPORTED, not fixed
- [ ] Difference table in the completion report
- [ ] `plans/README-claude-fable.md` status row updated

## STOP conditions

Stop and report back (do not improvise) if:

- The step-1 table shows the blocks differ MORE than superficially (majority
  of rows DRIFT) — the "triplicated" premise is wrong and extraction would
  launder three behaviors into one; report the table and await direction.
- The resolver needs state not reachable from plug-core without new plumbing
  through the engine handle (more than passing existing registry borrows) —
  report the dependency knot.
- Preserving a DRIFT cell requires the shared API to grow a per-transport
  flag for more than 2 behaviors — the abstraction is failing; report.
- The parity matrix fails on a migration in a way the difference table
  predicted (drift was load-bearing) — report; choosing which behavior is
  correct is an operator decision.

## Maintenance notes

- New notification types are now added in ONE place (`classify`/`resolve`)
  plus at most a delivery arm per transport — put a comment at each delivery
  site pointing to `fanout.rs`.
- **Plan 016 depends on this landing first**: it moves the daemon's (now
  thin) fanout site into a submodule. If 016 executes before 015 for some
  reason, 015's daemon migration happens in the new module location — update
  the drift-check paths accordingly.
- Plan 017's dispatch-unification design should treat `fanout.rs` as the
  notification half of the eventual shared dispatcher — reference it there.
- Review focus: behavior preservation per the difference table; no transport
  types leaked into plug-core's new module.
