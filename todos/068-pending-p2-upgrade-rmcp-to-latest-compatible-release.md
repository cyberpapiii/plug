---
status: pending
priority: p2
issue_id: "068"
tags: [dependency, rmcp, mcp, maintenance, compatibility]
dependencies: []
---

# Upgrade rmcp to the latest compatible release

## Problem Statement

The workspace is currently pinned to `rmcp = 1.1.0` in [Cargo.toml](/Users/robdezendorf/Documents/GitHub/plug/Cargo.toml). `plug` now relies heavily on newer MCP surfaces such as Tasks, richer tool metadata, and protocol-version-sensitive behavior.

Staying pinned indefinitely increases the risk that repo-local workarounds drift from upstream SDK behavior or that newer MCP features remain harder to adopt than necessary.

## Findings

- Current workspace dependency is `rmcp = 1.1.0` in [Cargo.toml](/Users/robdezendorf/Documents/GitHub/plug/Cargo.toml).
- Today’s work landed and passed the full suite on top of `rmcp 1.1.0`, so there is no urgent blocker.
- An unlocked local reinstall path (`cargo install --path plug --force`) drifted to newer compatible crates and pulled `rmcp 1.3.0`, which failed to compile against current `main` because several upstream types are now `#[non_exhaustive]` and `plug` still constructs them directly in multiple call sites.
- The immediate contributor-facing install regression was mitigated by switching [scripts/dev-reinstall.sh](/Users/robdezendorf/Documents/GitHub/plug/scripts/dev-reinstall.sh) to `cargo install --path plug --force --locked`, but that does not remove the underlying SDK-upgrade pressure.
- The codebase now uses a broad slice of MCP functionality:
  - Tasks
  - streamable HTTP client/server
  - auth
  - elicitation
  - sampling
  - richer metadata surfaces
- Because those paths are central, an `rmcp` bump should be treated as a dedicated dependency-migration task, not mixed into unrelated feature work.

## Proposed Solutions

### Option 1: Dedicated upgrade branch with latest `1.x`

**Approach:** Bump `rmcp` and `rmcp-macros` via Cargo to the latest compatible `1.x` release, then fix compile/runtime changes and run the full suite plus targeted transport/auth/tasks integration checks.

**Pros:**
- Lowest-risk path
- Keeps the migration attributable and reviewable
- Likely captures useful upstream fixes without large API churn

**Cons:**
- Still may require code changes across transport/auth/task paths

**Effort:** 3-6 hours

**Risk:** Medium

---

### Option 2: Broader dependency refresh

**Approach:** Upgrade `rmcp` together with adjacent MCP-related dependencies in one maintenance pass.

**Pros:**
- Fewer dependency-upgrade windows
- Can align related packages together

**Cons:**
- Harder to isolate regressions
- More review and verification burden

**Effort:** 6-10 hours

**Risk:** High

## Recommended Action

Take **Option 1** as a dedicated maintenance branch when dependency work is next in scope.
Keep the priority at `p2` for now because `main` is stable and the local reinstall path is now locked, but treat this as the next MCP dependency task rather than open-ended backlog.

## Technical Details

**Affected files:**
- [Cargo.toml](/Users/robdezendorf/Documents/GitHub/plug/Cargo.toml)
- [Cargo.lock](/Users/robdezendorf/Documents/GitHub/plug/Cargo.lock)
- likely transport/auth/task call sites across `plug-core` and `plug`

**Related components:**
- MCP Tasks support
- streamable HTTP transport
- upstream/downstream auth
- protocol-version negotiation

## Resources

- **Current pin:** `rmcp = 1.1.0`
- **Suggested verification:** `cargo test --workspace --quiet` plus targeted task/auth/HTTP/IPC integration checks

## Acceptance Criteria

- [ ] Workspace is upgraded to the latest compatible `rmcp` release intentionally chosen for `plug`
- [ ] Full workspace tests pass
- [ ] Targeted task/auth/HTTP/IPC integration coverage still passes
- [ ] Any behavior changes from the SDK upgrade are documented in truth docs or solution docs as needed

## Work Log

### 2026-03-22 - Follow-up captured

**By:** Codex

**Actions:**
- Checked the current workspace pin in [Cargo.toml](/Users/robdezendorf/Documents/GitHub/plug/Cargo.toml)
- Decided the upgrade should be tracked separately from today’s completed task-support work
- Recorded a dedicated dependency-upgrade todo

**Learnings:**
- The repo is stable on `rmcp 1.1.0` today
- The upgrade is worth doing, but only as isolated maintenance work

### 2026-03-30 - Compatibility pressure confirmed

**By:** Codex

**Actions:**
- Verified that the repo still pins `rmcp = 1.1.0`
- Observed an unlocked local reinstall drift into `rmcp 1.3.0`
- Confirmed that newer `rmcp` surfaces now cause compile failures on current `main`
- Landed a separate fix to make [scripts/dev-reinstall.sh](/Users/robdezendorf/Documents/GitHub/plug/scripts/dev-reinstall.sh) use `--locked`

**Learnings:**
- The current `rmcp` pin is stable, but the upgrade pressure is no longer hypothetical
- The SDK bump should stay isolated from feature work, but it now has concrete evidence behind it
