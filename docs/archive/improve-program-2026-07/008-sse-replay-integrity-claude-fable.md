# Plan 008: Fix SSE event-replay loss and the raced-sender enqueue gap in the stateful session store

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README-claude-fable.md`.
>
> **Drift check (run first)**: `git diff --stat e341625..HEAD -- plug-core/src/session/stateful.rs`
> If the file changed since this plan was written, compare the "Current
> state" excerpts against the live code before proceeding; on a mismatch,
> treat it as a STOP condition. Another AI agent (Codex) may be working in
> this repo concurrently.

## Status

- **Priority**: P2
- **Effort**: M
- **Risk**: MEDIUM (session/notification delivery path for HTTP clients)
- **Depends on**: none (plan 003 touches the same FILE at `:482` — a one-line
  metrics fix in `evict_expired`. If plan 003 merged first, expect that diff;
  it does not overlap these functions. If executing both yourself, do 003
  first, it's trivial.)
- **Category**: correctness
- **Planned at**: commit `e341625`, 2026-07-11

## Why this matters

The stateful session store backs SSE delivery for downstream HTTP clients
(`plug serve`). It has a replay mechanism (`Last-Event-ID` resumption): while
a client is disconnected, events are queued; when it reconnects, the queue is
drained to the new SSE stream. Two defects break its integrity guarantee:

1. **Replay drain drops the tail** (`send_replay_events`): the pending queue
   is drained ENTIRELY up front; events are then sent one by one; on the
   first send failure only THAT event is re-enqueued and the loop `break`s —
   every event after it (already drained, not yet sent) is silently
   discarded. A client that reconnects and immediately drops (flaky network —
   precisely the client that relies on replay) permanently loses the tail of
   its queue, and events are re-delivered out of order relative to the
   re-enqueued one.

2. **Raced-sender enqueue gap** (`send_or_enqueue`): when there is no live
   sender the event is enqueued blindly — but when a sender IS present and
   the send fails (receiver just dropped), the failed event is handled via
   the raced-sender path while the queue-bound path never re-checks whether a
   NEW sender was installed between the "no sender" check and the enqueue.
   The store already solves exactly this race for sender-clobbering with
   `clear_sender_if_matching` (the 004-plan merge, commit `98281c8` "SSE
   sender clobber race") — the enqueue side needs the mirror-image check.

## Current state

All excerpts verified at commit `e341625` in `plug-core/src/session/stateful.rs`.

The blind enqueue (`:204-211`):

```rust
// :204-211
let sender = { session.sse_sender.read().clone() };
match sender {
    Some(tx) => {
        if tx.send(event.clone()).await.is_err() {
            // receiver dropped; fall through to raced-sender handling
            ...
        }
    }
    None => {
        session.enqueue_pending(event); // <- no recheck: a sender installed
                                        //    between the read() and here never
                                        //    sees this event until NEXT reconnect
    }
}
```

(Read `:180-230` for the exact surrounding shape — the excerpt above is
abbreviated; match against the real code. The key fact: the `None` arm
enqueues without re-checking the sender slot, and nothing later flushes the
queue while a sender remains connected.)

The lossy drain (`:279-296`):

```rust
// :279-296  send_replay_events
let pending = session.drain_pending(); // takes the WHOLE queue
for event in pending {
    if tx.send(event.clone()).await.is_err() {
        session.enqueue_pending(event); // only the FAILED event survives
        break;                          // the rest of `pending` is dropped here
    }
}
```

Context you need:

- `enqueue_pending` / `drain_pending` are methods on the session entry —
  read their definitions (search `fn enqueue_pending` / `fn drain_pending`
  in the same file) to learn the queue type (VecDeque or Vec) and any
  bounded-capacity behavior (if the queue is bounded, re-enqueueing many
  events may evict — see STOP conditions).
- `clear_sender_if_matching` (search the file) is the existing raced-sender
  guard from the sender-clobber fix — read it; your `None`-arm fix mirrors
  its compare-and-act pattern.
- Metrics: check whether dropped/enqueued events are counted (search
  `metrics`/`counter` in the file) and keep counts accurate after the fix.
- Tests: the file has a `#[cfg(test)]` module and there are session/SSE
  integration tests — `grep -rn 'replay\|last_event\|Last-Event' plug-core/src plug-core/tests plug/tests` to find the existing replay tests to pattern-match.

## Fix design (already decided)

**Fix 1 — drain integrity.** On send failure, re-enqueue the failed event
AND all not-yet-sent events, preserving order, at the FRONT of the queue
(events that arrived while draining must stay behind them):

```rust
let pending = session.drain_pending();
let mut iter = pending.into_iter();
while let Some(event) = iter.next() {
    if tx.send(event.clone()).await.is_err() {
        // Put back the failed event followed by the untouched tail, ahead of
        // anything enqueued concurrently during the drain.
        session.requeue_front(std::iter::once(event).chain(iter));
        break;
    }
}
```

This needs a new `requeue_front` method next to `enqueue_pending` (a
`VecDeque` makes it natural; if the queue is a `Vec`, splice at index 0). If
concurrent enqueue during drain is impossible because a lock is held across
the whole drain-and-send (check!), plain re-append also preserves order —
verify which holds and implement accordingly, saying which in the commit
message. Do NOT hold a synchronous lock across `tx.send(...).await` — if the
current code drains under a guard and releases before sending, keep that
structure.

**Fix 2 — enqueue recheck.** In the `None` arm: enqueue, then re-read the
sender slot; if a sender is NOW present, attempt the replay drain (call the
same `send_replay_events`-style flush) so the racing event isn't stranded:

```rust
None => {
    session.enqueue_pending(event);
    // Mirror of clear_sender_if_matching: a sender may have been installed
    // between the read() above and the enqueue — flush so the event isn't
    // stranded until the next reconnect.
    if let Some(tx) = session.sse_sender.read().clone() {
        // reuse the (now loss-safe) replay flush
        send_replay_events(&session, &tx).await;
    }
}
```

Adjust names/signatures to the real code. The double-flush case (reconnect
replay already running) must be idempotent — both paths go through
drain/requeue, so a concurrent flush sees an empty queue; confirm
`drain_pending` is atomic (single lock acquisition).

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Targeted tests | `cargo test -p plug-core session` | all pass |
| Full tests | `cargo test --workspace` | all pass |
| Lint | `cargo clippy --workspace --all-targets -- -D warnings` | exit 0 |
| Format | `cargo fmt --check` | exit 0 |

## Scope

**In scope**:
- `plug-core/src/session/stateful.rs` — `send_or_enqueue` (or the real name
  of the `:204` function), `send_replay_events`, a new `requeue_front`
  helper, and tests.

**Out of scope** (do NOT touch):
- `clear_sender_if_matching` and the sender-install path — already fixed
  (commit `98281c8`); you only MIRROR its pattern.
- `evict_expired` (`:482`) — plan 003's one-liner.
- The stateless session store, HTTP server (`http/server.rs`), and SSE wire
  encoding.
- Queue capacity/backpressure policy — keep existing bounds exactly.

## Git workflow

- Branch: `fix/sse-replay-integrity`
- Commit: `fix(session): preserve replay queue tail on send failure and re-check sender after enqueue`.
- Do NOT push or open a PR unless the operator instructed it.

## Steps

### Step 1: Read the real functions and confirm the two defects

Read `stateful.rs:180-320` plus the definitions of `enqueue_pending`,
`drain_pending`, `clear_sender_if_matching`, and the queue field. Confirm:
(a) the drain-then-break tail loss; (b) the un-rechecked `None`-arm enqueue;
(c) whether a lock is held across the drain+send (determines requeue_front
vs re-append); (d) whether the queue is bounded.

**Verify**: you can state (a)–(d) concretely; if (a) or (b) is NOT present as
described, STOP (see STOP conditions).

### Step 2: Implement Fix 1 (drain integrity)

Add `requeue_front` (or the re-append variant if step 1(c) showed the lock
covers the whole operation) and rewrite the failure arm of
`send_replay_events` per the design. Keep any metrics counters accurate
(events re-enqueued are not "delivered").

**Verify**: `cargo check --workspace` → exit 0.

### Step 3: Implement Fix 2 (enqueue recheck)

Per the design, in the `None` arm. Ensure no lock is held across the awaited
flush.

**Verify**: `cargo check --workspace` → exit 0.

### Step 4: Tests

In the file's existing `#[cfg(test)]` module (pattern-match the existing
session tests found in step 1):

1. `replay_send_failure_preserves_tail` — queue 5 events; give the session a
   sender whose receiver is dropped after receiving 2; run the replay flush;
   assert the queue now holds events 3,4,5 in order (nothing lost, order kept).
2. `replay_requeue_orders_before_concurrent_enqueues` — only if step 1(c)
   showed concurrent enqueue during drain is possible: simulate it and assert
   the requeued tail sits ahead of the newly enqueued event. (If impossible
   by lock structure, skip and say so in the completion report.)
3. `enqueue_recheck_flushes_to_new_sender` — no sender → call send_or_enqueue
   with a sender installed between (drive the race deterministically: install
   the sender, but call the function through a path where the initial read
   sees None — easiest is to make the test call the internal pieces in the
   racy order; if the function can't be decomposed for the test, install the
   sender from another task with a yield and loop the test 100 iterations).
   Assert the event arrives at the receiver without a reconnect.
4. `replay_empty_queue_is_noop` — flush with empty queue; no panic, no send.

**Verify**: `cargo test -p plug-core session` → all pass; `cargo test --workspace` → all pass.

## Test plan

Covered by step 4. The tests live next to the code (unit level) because the
race windows need direct access to the session entry; if an integration-level
`Last-Event-ID` replay test exists (step 1 grep), run it explicitly and
mention it in the completion report.

## Done criteria

- [ ] `cargo test --workspace` exits 0
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` exits 0
- [ ] `cargo fmt --check` exits 0
- [ ] `replay_send_failure_preserves_tail` proves no tail loss
- [ ] Only `plug-core/src/session/stateful.rs` modified (`git status`)
- [ ] `plans/README-claude-fable.md` status row updated

## STOP conditions

Stop and report back (do not improvise) if:

- Step 1 shows either defect is not present as described (code drifted or the
  planning read was wrong) — report what the code actually does.
- The pending queue is BOUNDED and requeue_front could evict newer events —
  report the capacity policy conflict; choosing what to drop is an operator
  decision.
- Fixing the `None` arm requires restructuring the function's lock scope in a
  way that affects the already-fixed sender-clobber path — report; do not
  risk regressing commit `98281c8`.
- Test 3's race cannot be driven deterministically OR probabilistically
  (100-iteration loop stays green against an unfixed build — i.e. the test
  can't demonstrate the bug it guards) — land fixes 1+2 with tests 1,2,4 and
  report test 3 as not-demonstrable.

## Maintenance notes

- `requeue_front` + `drain_pending` are now a matched pair — any future
  change to queue discipline must keep "drain is all-or-requeue" or replay
  loses its integrity guarantee again.
- The enqueue-recheck mirrors `clear_sender_if_matching`; if the sender-slot
  type changes (e.g. to a watch channel), both sites change together.
- Review focus: no `.await` while holding the queue/sender lock; ordering of
  requeued tail vs concurrent enqueues.
