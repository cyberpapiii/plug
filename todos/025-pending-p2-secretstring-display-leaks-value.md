---
status: pending
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

- [ ] `format!("{}", secret_string)` returns `[REDACTED]`
- [ ] `format!("{:?}", secret_string)` returns `[REDACTED]`
- [ ] `.as_str()` still returns the raw value for legitimate use
