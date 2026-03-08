---
title: Review fixes TLS backend portability
status: completed
date: 2026-03-07
---

# Review Fixes TLS Backend Portability

## What We're Building

A narrow follow-up to PR 21 that restores green CI without undoing the review fixes. The scope is limited to the outbound HTTP upstream TLS backend and license policy so the review-fix branch can build cleanly on the existing cross-check targets.

## Why This Approach

The previous switch from `reqwest-native-tls` to rmcp's default rustls path removed `openssl-sys`, but it introduced `aws-lc-sys` and `webpki-root-certs`, which caused two new failures:
- `cargo deny` rejected `OpenSSL` and `CDLA-Permissive-2.0`
- cross-target checks still had native crypto compilation pressure through `aws-lc-sys`

The smallest correct fix is:
- use rmcp's `reqwest-tls-no-provider` path
- install the `ring` crypto provider explicitly from our code
- allow only `CDLA-Permissive-2.0` in `deny.toml`

This keeps us off both OpenSSL and aws-lc while preserving HTTPS upstream support.

## Options Considered

### Option 1: Widen `deny.toml` for OpenSSL and keep current rustls/aws-lc path

Rejected. It accepts licenses and native crypto dependencies we do not need.

### Option 2: Revert to `reqwest-native-tls`

Rejected. It reintroduces the original cross-compilation failure mode through `openssl-sys`.

### Option 3: Use `reqwest-tls-no-provider` plus explicit `ring`

Chosen. It is the narrowest change that removes OpenSSL/aws-lc while keeping runtime HTTPS behavior under our control.

## Key Decisions

- Keep the fix on `feat/review-fixes-critical`; do not open a new branch for a patch to the active review-fix PR.
- Treat this as a dependency/runtime policy fix, not just CI paperwork.
- Install the crypto provider at the actual HTTP upstream connection boundary in `plug-core`, so tests and daemon paths are covered without extra binary startup hooks.
- Add `CDLA-Permissive-2.0` only if it remains the sole new license after the provider switch.

## Constraints

- Do not add blanket tool-call rate limiting.
- Do not reintroduce OpenSSL or native-tls.
- Do not widen the license allowlist beyond what is actually required.
- Preserve authenticated HTTPS upstream behavior.

## Success Criteria

- `cargo deny check licenses` passes
- `cargo test` passes
- `cargo clippy --all-targets --all-features -- -D warnings` passes
- CI cross-checks no longer fail because of OpenSSL/aws-lc native crypto linkage
- PR 21 remains scoped to review-fix and portability work only

## Open Questions

None. The remaining uncertainty is purely technical validation during implementation.
