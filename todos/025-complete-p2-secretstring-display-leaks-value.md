---
status: complete
priority: p2
issue_id: "025"
tags: [code-review, security]
dependencies: []
---

# SecretString Display trait exposes raw secret value

## Problem Statement

`SecretString` correctly redacts in `Debug` output but the `Display` impl writes the raw secret. Any code using `format!("{}", token)`, `tracing::info!(%token)`, or `.to_string()` would leak credentials. Currently not exploitable (auth_token is accessed via `.as_str()`), but is a latent defect.

## Findings

- **Source**: security-sentinel
- **Location**: `plug-core/src/types.rs:22-26`
- **Evidence**: `Display` impl uses `write!(f, "{}", self.0)` — exposes raw value. `Debug` impl correctly writes `[REDACTED]`.

## Proposed Solutions

### Option A: Redact in Display (Recommended)
Change `Display` to write `[REDACTED]`, matching `Debug` behavior. Raw access remains via `.as_str()`.
- **Pros**: One-line fix, prevents future accidents
- **Cons**: None
- **Effort**: Small (1 line)

## Acceptance Criteria

- [x] `format!("{}", secret_string)` returns `[REDACTED]`
- [x] `format!("{:?}", secret_string)` returns `[REDACTED]`
- [x] `.as_str()` still returns the raw value for legitimate use

## Work Log

### 2026-03-06 - Completed During v0.1 Stabilization

**By:** Codex

**Actions:**
- Updated `Display` redaction in [types.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/types.rs)
- Added regression tests for both `Debug` and `Display` redaction paths
- Verified with focused `cargo test -p plug-core types::tests`

**Learnings:**
- This was a true latent defect rather than an active exploit path, but it was low-cost and worth eliminating immediately.
