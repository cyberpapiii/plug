---
status: pending
priority: p3
issue_id: "069"
tags: [macos, codesign, notarization, release, distribution, developer-experience]
dependencies: []
---

# Sign and notarize release binaries so macOS users never see recurring Keychain prompts

## Problem Statement

plug stores upstream OAuth credentials in the macOS login Keychain. The macOS
Keychain "Always Allow" ACL binds to the binary's **code signature**. Binaries
that ship today — `cargo install plug-mcp`, the Homebrew tap, and the GitHub
release downloads — are **ad-hoc signed** (their signature is just a per-build
hash), so the approval never persists: macOS re-prompts on every update and
across the many short-lived `plug connect` processes. This is friction every
macOS user hits once they configure OAuth upstreams.

A local workaround already exists for self-built installs (`plug codesign-setup`
and `scripts/setup-codesigning.sh` install a stable *self-signed* identity; the
`codesign_identity` doctor check nudges users toward it). But self-signed
identities are per-machine and require a manual one-time run with a login-password
dialog. They do **not** help a user who installs a release binary the normal way
and never discovers the command.

The complete fix is to ship release binaries that are already **signed with an
Apple Developer ID and notarized**, so the signature is stable and trusted out of
the box and no user ever runs a setup step or sees a recurring prompt.

## Findings

- Root cause and the self-signed local fix are documented in
  [docs/solutions/integration-issues/local-codesigning-identity-stops-keychain-reprompts.md](/Users/robdezendorf/Documents/GitHub/plug/docs/solutions/integration-issues/local-codesigning-identity-stops-keychain-reprompts.md).
- The self-signed path covers only the local-dev / clone install flow:
  - `plug codesign-setup` (built-in, install-path-agnostic, signs the running binary)
  - `scripts/setup-codesigning.sh` (idempotent one-time setup)
  - `scripts/dev-reinstall.sh` (auto re-signs after `cargo install`)
  - `plug doctor` → `codesign_identity` check warns when ad-hoc + OAuth upstreams present
- Release/distribution channels remain ad-hoc:
  - crates.io `cargo install plug-mcp`
  - Homebrew tap `cyberpapiii/tap/plug`
  - GitHub release archives + the shell installer
- A real fix requires a **paid Apple Developer account** (Developer ID Application
  cert), signing in CI, and notarization via `notarytool` + stapling. This is a
  release-pipeline change, not application code.

## Proposed Solutions

### Option 1: Developer ID signing + notarization in the release pipeline (complete fix)

**Approach:** Add a macOS signing + notarization step to the release workflow:
sign the built binary with a Developer ID Application certificate (stored as a CI
secret), submit to Apple with `xcrun notarytool submit --wait`, and staple the
ticket. Apply to the GitHub release archives and the Homebrew bottle. (crates.io
ships source, so `cargo install plug-mcp` compiles locally and stays ad-hoc — the
doctor nudge / `plug codesign-setup` remains the answer there.)

**Pros:**
- Invisible for the majority of macOS users (release + Homebrew) — no setup, no prompt.
- Stable, trusted signature; also smooths Gatekeeper for any future GUI surfaces.

**Cons / requirements:**
- Needs a paid Apple Developer account and a Developer ID cert in CI secrets.
- Adds notarization latency and complexity to releases.
- Does not cover source installs (`cargo install` from crates.io) — those still rely on the local fix.

### Option 2: Keep the self-signed local fix only (status quo)

**Approach:** Ship nothing new; rely on `plug codesign-setup` + the doctor nudge.

**Pros:**
- Zero release-pipeline change; already implemented.

**Cons:**
- Release/Homebrew users still get recurring prompts unless they find and run the command.

## Recommendation

Defer until plug has a real macOS install audience beyond local dev. When that
happens, do **Option 1** for the GitHub-release and Homebrew channels, and keep
`plug codesign-setup` + the doctor nudge as the answer for source (`cargo install`)
installs.
