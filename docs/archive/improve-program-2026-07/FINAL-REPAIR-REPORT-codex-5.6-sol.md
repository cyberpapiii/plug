# Final repair report - Codex 5.6 sol

Date: 2026-07-12

Status: `done on main`. Code commit `4e07fbd` and this report reached `main`
through the reviewed `improve/integration` line on 2026-07-12.

## What this pass fixed

The final counter-review found three defects in the wave-3 repairs and one
missing proof. Commit `4e07fbd` fixes them.

1. Equivalent subscription rebinds could supersede each other. The first
   waiter then returned `Ok` while the winning migration failed and removed
   the entry. Pending entries now record their intended owner, and equivalent
   rebinds share one watch result. The downstream response accepts only an
   Active entry on the published owner. The existing routeless grace case is
   preserved and still requires an Active entry.
2. `SubscriptionRegistry` owned a post-confirm hook that captured the same
   registry through `Arc`. That cycle retained the registry, route cache, and
   server-manager graph after `ToolRouter` dropped. The hook now captures a
   `Weak` registry reference.
3. Native task creation timed out only after rmcp returned a request handle.
   A full 1,024-message peer queue could block the send forever while the
   detached task retained its owner-create guard. One deadline now covers
   request-handle acquisition and response receipt. The existing bounded
   cancellation and late-result reaper still apply after a request id exists.
4. The wave-3 report claimed a full HTTP POST-versus-DELETE regression, but
   the branch had lower-layer tests only. This pass adds an Axum handler test
   that parks the task-wrapped POST at its post-guard session validation,
   runs DELETE through `/mcp`, and proves the late create is refused with no
   task record.

## Regression proof

The three behavioral tests failed against the wave-3 code before the fixes:

- `equivalent_concurrent_rebinds_share_authoritative_failure`
- `dropping_router_releases_subscription_registry`
- `native_enqueue_bounds_request_handle_acquisition`

The new handler test is
`task_wrapped_post_racing_delete_refuses_late_create`. All four tests passed
three consecutive runs after the fixes, 12 of 12 total.

The complete branch gate passed after the code change:

- `cargo test --workspace`: 857 tests, split as 620 plug-core library, 45
  integration, and 192 binary tests
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo fmt --all -- --check`
- `cargo +1.88.0 check --workspace --all-targets`
- `cargo deny check advisories`
- `scripts/check-todo-status.sh`

## Scope boundary

This closes every concrete defect from the final wave-3 counter-review. The
optional follow-up list in Fable's execution report remains a follow-up list.
In particular, this commit does not change cross-owner subscription supersede
semantics or add timeouts to all resource subscribe and unsubscribe calls.
Those are older, documented design limits and were not required to correct the
false-success, retention, native-send, or HTTP-proof defects above.
