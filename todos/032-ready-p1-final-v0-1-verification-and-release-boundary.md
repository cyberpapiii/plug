---
status: ready
priority: p1
issue_id: "032"
tags: [v0-1, verification, release]
dependencies: ["029", "030", "031"]
---

# Final `v0.1` verification and release boundary

## Problem Statement

The stabilization work needs an explicit release boundary. Without a final verification pass, the project risks drifting directly into Phase 2 without a trustworthy `v0.1` checkpoint.

## Findings

- The code stabilization tranche is already landing in separate commits
- Full-suite verification has not yet been run after the latest changes
- The docs gate should complete before release tagging

## Proposed Solutions

### Option 1: Run a strict verification boundary and stop (Recommended)

**Approach:** After remaining `v0.1` truth/doc tasks land, run format, clippy, full tests, and a small manual smoke matrix before deciding on a tag.

**Pros:**
- Clean finish line
- Prevents Phase 2 scope bleed

**Cons:**
- Slower than “just keep coding”

**Effort:** Small

**Risk:** Low

## Recommended Action

Use this as the explicit last task in the `v0.1` sequence. If verification passes, tag candidate `v0.1.0`. If it fails, fix only release blockers and stop.

## Acceptance Criteria

- [ ] `cargo fmt --check` passes
- [ ] `cargo clippy --all-targets --all-features -- -D warnings` passes
- [ ] `cargo test` passes
- [ ] Manual smoke checks for `plug start`, `plug status`, `plug serve`, and `plug connect` complete
- [ ] Core docs are already rewritten before this step starts

## Work Log

### 2026-03-06 - Created During v0.1 Task Planning

**By:** Codex

**Actions:**
- Created as the final gate after narrowing the strategic plan into `v0.1` execution work

**Learnings:**
- The most important release discipline here is to stop after `v0.1` instead of letting Phase 2 work begin opportunistically
