# Plug App Distribution Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship one Developer ID-signed, notarized, stapled Plug DMG with Sparkle updates and an identical Homebrew cask artifact.

**Architecture:** The release workflow builds the universal app from the same tagged Rust binaries used by CLI packages, signs nested code inside-out, notarizes and staples the app and DMG, generates a signed full-history Sparkle appcast, then publishes all release assets atomically. Existing CLI release paths remain fail-closed and unchanged in behavior.

**Tech Stack:** GitHub Actions, Xcode, codesign, notarytool, hdiutil, Sparkle 2, EdDSA appcast signatures, Homebrew cask.

**Spec:** `docs/superpowers/specs/2026-08-25-plug-macos-app-design.md`

## Global Constraints

- DMG is the recommended app channel; CLI `.pkg`, archives, formula, and installer remain.
- App and DMG must both be notarized and stapled.
- Every release carries a regenerated full-history `appcast.xml`.
- Sparkle and Developer ID private keys remain separate.
- The cask uses the exact release DMG and declares `auto_updates true`.
- Any missing credential or failed verification aborts before release publication.

---

### Task 1: Add Sparkle with a compatibility-aware update coordinator

**Files:**
- Modify: `PlugApp/PlugApp.xcodeproj/project.pbxproj`
- Create: `PlugApp/PlugApp/Updates/UpdateCoordinator.swift`
- Modify: `PlugApp/PlugApp/Info.plist`
- Create: `PlugApp/PlugAppTests/UpdateCoordinatorTests.swift`

**Interfaces:**
- Produces: `UpdateCoordinator.checkForUpdates()`, stable feed URL, restart-required state.

- [ ] **Step 1: Write tests for update completion and daemon skew**

Use a fake updater and daemon handshake. Prove app replacement never restarts silently and always offers `Restart daemon to finish` when the bundled version is newer.

- [ ] **Step 2: Add the current stable Sparkle 2 package after verifying its official release**

Pin an exact 2.x version in the Xcode project. Set `SUFeedURL` to `https://github.com/cyberpapiii/plug/releases/latest/download/appcast.xml`, `SUPublicEDKey` from a generated public key, and automatic checks on with user-controllable settings.

- [ ] **Step 3: Run tests and commit**

```bash
xcodebuild test -project PlugApp/PlugApp.xcodeproj -scheme PlugApp -destination 'platform=macOS'
git add PlugApp
git commit -m "feat: add signed Sparkle updates"
```

### Task 2: Build, sign, notarize, and staple the app and DMG

**Files:**
- Create: `scripts/build-macos-app.sh`
- Create: `scripts/sign-notarize-macos-app.sh`
- Create: `scripts/create-macos-dmg.sh`
- Modify: `.github/workflows/release.yml`
- Modify: `docs/RELEASING.md`

**Interfaces:**
- Produces: `Plug.app`, `plug-mcp-<version>.dmg`, signature/notarization verification logs.

- [ ] **Step 1: Add script contract tests**

Run each script with missing inputs and assert exit 2 plus a named missing variable/file. Test that the packaging script refuses an unsigned nested `plug` binary.

- [ ] **Step 2: Build a universal app from both signed Rust architectures**

Use `lipo -create` only for the two same-tag `plug` binaries, place the result at `Plug.app/Contents/Resources/plug`, and verify `lipo -info` reports arm64 and x86_64.

- [ ] **Step 3: Sign nested binary then app**

The signing script imports the P12 into the existing ephemeral keychain pattern, signs the nested binary first, then frameworks, then app with hardened runtime and timestamp. Run `codesign --verify --deep --strict` and inspect entitlements.

- [ ] **Step 4: Notarize/staple app, create DMG, notarize/staple DMG**

Submit a ZIP of the app, wait for Accepted, staple/validate app, create a read-only styled DMG with Applications link, submit the DMG, staple/validate it, and run `spctl --assess --type open --context context:primary-signature` plus codesign checks.

- [ ] **Step 5: Wire release workflow and documentation**

Add a macOS app job depending on both Darwin binaries. Upload app/DMG only after all verification. Document exact secret names, artifact checks, and why bare CLI binaries cannot be stapled.

- [ ] **Step 6: Commit**

```bash
git add scripts/build-macos-app.sh scripts/sign-notarize-macos-app.sh scripts/create-macos-dmg.sh .github/workflows/release.yml docs/RELEASING.md
git commit -m "build: package notarized Plug macOS app"
```

### Task 3: Generate and publish the full-history appcast

**Files:**
- Create: `scripts/generate-appcast.sh`
- Modify: `.github/workflows/release.yml`
- Modify: `docs/RELEASING.md`

**Interfaces:**
- Produces: signed `appcast.xml` attached to every release.

- [ ] **Step 1: Add fixture tests for history preservation**

Given two prior releases plus a new DMG, assert output has all three ordered entries, valid enclosure URLs, version/build numbers, lengths, EdDSA signatures, and release notes links.

- [ ] **Step 2: Generate the key using Sparkle tooling**

Store the private key in Personal 1Password and GitHub Actions as `SPARKLE_EDDSA_PRIVATE_KEY`; commit only the public key through `SUPublicEDKey`. Never print the private key or place it in a shell argument.

- [ ] **Step 3: Generate from GitHub release history in CI**

Download existing appcast/DMGs, add the current DMG, regenerate full history with Sparkle tooling, validate XML and every signature, then attach `appcast.xml` even if the release contains no changed app build.

- [ ] **Step 4: Commit**

```bash
git add scripts/generate-appcast.sh .github/workflows/release.yml docs/RELEASING.md PlugApp/PlugApp/Info.plist
git commit -m "build: publish full-history Sparkle appcast"
```

### Task 4: Publish the identical Homebrew cask

**Files:**
- Modify: `.github/workflows/release.yml`
- Modify: `docs/RELEASING.md`

**Interfaces:**
- Produces: `Casks/plug.rb` in `cyberpapiii/homebrew-tap` using the release DMG.

- [ ] **Step 1: Generate and syntax-test the cask**

The cask uses the stable tagged DMG URL, exact SHA-256, `app "Plug.app"`, `auto_updates true`, and an uninstall stanza invoking the app's service cleanup command before trashing the bundle.

- [ ] **Step 2: Verify artifact identity**

Compare the cask download SHA-256 with the GitHub release DMG before pushing the tap commit. There is no cask-specific build.

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/release.yml docs/RELEASING.md
git commit -m "build: publish Plug app Homebrew cask"
```

### Task 5: Release, install, and verify on the owner Mac

**Files:**
- Modify: `CHANGELOG.md`
- Create: `docs/RELEASE-NOTES-0.5.0.md`
- Modify: `docs/PROJECT-STATE-SNAPSHOT.md`
- Modify: `docs/PLAN.md`

**Interfaces:**
- Produces: public release, installed Plug.app, app-managed daemon, user-friendly notes.

- [ ] **Step 1: Run all Rust, Swift, app, signing, and workflow lint gates**

Expected: every gate exits 0 at the exact release commit.

- [ ] **Step 2: Merge with a clean main and tag the verified commit**

Use a normal linear merge strategy without a merge commit when possible, push main, then create `v0.5.0` only after every required secret is confirmed present.

- [ ] **Step 3: Watch the release to completion and validate assets**

Require successful GitHub release, notarized DMG, `appcast.xml`, cask, CLI archives/packages, checksums, and no unsigned fallback.

- [ ] **Step 4: Install the DMG and adopt the daemon**

Use the app's first-run flow from the login session. Approve macOS prompts through the normal user-approved UI path, verify launchd ownership, binary version, socket handshake, Keychain access, real server/client state, and restart persistence.

- [ ] **Step 5: Verify offline Gatekeeper and Sparkle**

Validate stapled tickets on app and DMG, disconnect network for first launch verification, then publish a controlled prerelease update fixture and prove Sparkle verifies/downloads without manual replacement.

- [ ] **Step 6: Commit truth docs and clean repository state**

Delete merged branches/worktrees, confirm one clean main worktree synced with origin, and update release notes/current truth with only live-verified claims.

