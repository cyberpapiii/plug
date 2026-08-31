---
status: done
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

## Triage 2026-08-08 — superseded, see Retriage below

**By:** Claude Fable 5

Originally concluded the work was blocked on a paid Apple Developer account. That framing
was too narrow and is corrected in the next section. The two verified facts from it stand:

- `.github/workflows/release.yml` has no signing step, and a grep across `.github/` and
  `scripts/` for `notarytool`, `APPLE_ID`, or `DEVELOPER_ID` returns nothing, so there is
  no half-finished pipeline to complete.
- The local fallback is verified working on this machine: `plug doctor` reports
  `● codesign_identity  plug has a stable code-signing identity; Keychain approvals
  persist`.

## Retriage 2026-08-08 — an Apple Developer account is not required

**By:** Claude Fable 5

The original problem statement conflated two separate macOS mechanisms and let the harder
one set the bar for both.

**Keychain ACL persistence.** The "Always Allow" grant binds to the binary's designated
requirement, which for a signed binary is essentially "this identifier, signed by this leaf
certificate." Any *stable* certificate satisfies that. Nothing in the mechanism asks whether
the certificate chains to Apple. A self-signed identity fixes the recurring prompts exactly
as well as a Developer ID would — this is not a degraded workaround, it is a complete fix
for this problem. `plug codesign-setup` already implements it end to end: it creates the
identity on first run and signs whatever `plug` binary is executing, so it works the same
for `cargo install`, Homebrew, and release downloads.

**Gatekeeper.** Notarization is what a Developer ID buys, and it only matters for artifacts
carrying the `com.apple.quarantine` attribute. None of plug's current channels set it:
`curl` does not apply quarantine, Homebrew formula bottles are fetched the same way, and
`cargo install` compiles locally. (On Apple Silicon every binary must carry at least an
ad-hoc signature to execute at all, which the toolchain already produces, so nothing is
blocked from running.) The one channel where quarantine is plausible is the `.mcpb` bundle
from `scripts/build-mcpb.sh`, since a browser download would quarantine it and Claude
Desktop then launches the embedded binary.

So the correct split is: self-signing solves the problem this todo was actually opened for,
and Developer ID is a separate, smaller question scoped to browser-downloaded bundles.

### Option D (recommended): per-machine self-signed identity, surfaced at install time

Keep `plug codesign-setup` as the mechanism and fix the discovery gap, which was the real
complaint in the problem statement — not the absence of an Apple account. `install.sh` now
prints a macOS-specific line pointing at the command, alongside the existing `plug doctor`
`codesign_identity` nudge.

The installer only *suggests* the command rather than running it. `plug codesign-setup`
adds a code-signing trust root to the user's login keychain and pops a password dialog;
a piped `curl | sh` installer must not do either silently, and a password dialog the user
cannot attribute to anything is exactly the shape of a phishing prompt.

Cost: nothing. No account, no CI secret, no key to leak or rotate.

### Option E (rejected): sign in CI with a shared self-signed certificate

Technically this would work — a self-signed leaf produces a stable designated requirement
for every user without anyone running a setup step. It should still not be done.

The private key would have to live in repository secrets, where it is extractable by anyone
who can run a workflow or read the secret. A stolen key is worse here than in the usual
supply-chain case: an attacker could sign a malicious binary with the same identity, match
the designated requirement, and inherit every user's existing "Always Allow" grant for
plug's Keychain items — reading upstream OAuth credentials with no prompt at all. Unlike a
Developer ID certificate, a self-signed one has no revocation path that user machines
would honor, so there would be no way to invalidate it after the fact.

The per-machine identity in Option D has no such exposure: each key never leaves the
machine that generated it.

### Developer ID: still open, but rescoped

Option 1 above remains the answer *only* for quarantined artifacts, which today means the
`.mcpb` bundle if it is ever distributed as a browser download. It is no longer a
prerequisite for anything in this todo's problem statement. Revisit if a `.mcpb`, `.pkg`,
or GUI surface starts shipping.

**Status:** the discovery fix is implemented; the todo stays open only to track the
rescoped Developer ID question, which is deferred by choice.

## Closed 2026-08-30 — the rescoped Developer ID question is answered

**By:** Claude Opus 5

The retriage left this open only to track Developer ID signing for a quarantined
artifact, to be revisited "if a `.mcpb`, `.pkg`, or GUI surface starts shipping."
One did: Plug.app ships as a DMG through a Homebrew cask and Sparkle, and the
pipeline that builds it signs and notarizes.

Verified on `main` and on this machine:

- `.github/workflows/release.yml:185` runs `scripts/sign-notarize-macos-app.sh`,
  so signing and notarization happen in CI rather than by hand.
- `scripts/install-release.sh:64` runs `spctl --assess --type execute` on the
  staged app and refuses anything Gatekeeper rejects.
- `spctl --assess -vv /Applications/Plug.app` reports `source=Notarized Developer
  ID`, `origin=Developer ID Application: Robert Dezendorf (HJF7LN64XX)`, and
  `codesign -dv --verbose=4` reports `Notarization Ticket=stapled` with the
  hardened runtime enabled.
- `/Applications/Plug.app/Contents/Resources/plug` — the binary launchd runs as
  the daemon and that `plug connect` execs into — carries the same signature, so
  the Keychain ACL binds to a stable Developer ID rather than a per-build hash.

`cargo install plug-mcp` from crates.io still compiles locally and stays ad-hoc.
That is unchanged and out of scope by the retriage's own reasoning: `plug
codesign-setup` is the answer there, and `plug doctor`'s `codesign_identity`
check still nudges toward it.
