# Unified macOS Release and Migration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Publish one complete macOS Plug.app through the website, GitHub Releases, and Homebrew Cask while retaining standalone CLI distribution only for Linux and development.

**Architecture:** Release metadata comes from the workspace version. One gated release transaction stages the signed DMG, full-history appcast, app-only Cask, Linux Formula and archives, verifies their shared version/checksums, updates the tap, and only then promotes the GitHub release to latest.

**Tech Stack:** GitHub Actions, cargo-dist, Xcode, Sparkle, Homebrew, Bash, Developer ID signing, notarization.

**Spec:** `docs/superpowers/specs/2026-08-25-unified-macos-update-design.md`

## Global Constraints

- Runtime and app reconciliation plans must land before packaging removes any macOS standalone path.
- macOS public installs are DMG or Homebrew Cask; both install the same Plug.app.
- The Cask has no `binary` or `postflight` symlink writer.
- Appcast, DMG, and Cask publish as one gated release operation.
- Linux retains standalone Formula, shell installer, and archives.
- MCPB executable distribution is retired.
- Unknown user files and state are never removed by packaging scripts.

---

### Task 1: Make workspace version the release contract

**Files:**
- Modify: `PlugApp/PlugApp/Info.plist`
- Modify: `PlugApp/project.yml`
- Modify: `.github/workflows/release.yml`
- Create: `scripts/verify-release-contract.sh`
- Create: `scripts/test-release-contract.sh`

**Interfaces:**
- Produces `scripts/verify-release-contract.sh --tag <vX.Y.Z> --app <Plug.app> --appcast <appcast.xml> --cask <plug-app.rb>`.

- [ ] **Step 1: Write negative contract fixtures**

Pin failures for mismatched tag, app short version, embedded `plug --version`, appcast version, Cask version, and non-increasing build number.

- [ ] **Step 2: Verify tests fail**

```bash
bash scripts/test-release-contract.sh
```

- [ ] **Step 3: Implement one version source**

Read workspace version with `cargo metadata`. Use `$(MARKETING_VERSION)` for `CFBundleShortVersionString`, `$(CURRENT_PROJECT_VERSION)` for `CFBundleVersion`, and GitHub run number for the monotonic build. Compare all release surfaces before publication.

- [ ] **Step 4: Run and commit**

```bash
bash scripts/test-release-contract.sh
bash -n scripts/verify-release-contract.sh
xcodegen generate --spec PlugApp/project.yml
git add PlugApp .github/workflows/release.yml scripts/verify-release-contract.sh scripts/test-release-contract.sh
git commit -m "build: enforce one Plug release version"
```

### Task 2: Isolate development installs

**Files:**
- Modify: `scripts/dev-reinstall.sh`
- Modify: `scripts/setup-codesigning.sh`
- Modify: `plug/src/commands/codesign.rs`
- Modify: `docs/solutions/integration-issues/local-codesigning-identity-stops-keychain-reprompts.md`

- [ ] **Step 1: Add a temporary-home regression**

Run the installer against temporary `HOME` and `CARGO_HOME`; assert only `cargo/bin/plug-dev` appears and no `.local/bin/plug` is created or changed.

- [ ] **Step 2: Verify current behavior fails**

```bash
tmp="$(mktemp -d)"
HOME="$tmp/home" CARGO_HOME="$tmp/cargo" ./scripts/dev-reinstall.sh --quick
test -x "$tmp/cargo/bin/plug-dev"
test ! -e "$tmp/home/.local/bin/plug"
```

- [ ] **Step 3: Implement `plug-dev` behavior**

Install and sign `plug-dev`; run smoke tests with `PLUG_DEV=1`. Remove default production-path takeover. Make `plug codesign-setup` refuse a Developer ID app executable and operate only in verified development mode.

- [ ] **Step 4: Run and commit**

```bash
bash -n scripts/dev-reinstall.sh scripts/setup-codesigning.sh
cargo test -p plug-mcp commands::codesign
git add scripts/dev-reinstall.sh scripts/setup-codesigning.sh plug/src/commands/codesign.rs docs/solutions
git commit -m "build: isolate macOS development installs"
```

### Task 3: Retire MCPB executable distribution

**Files:**
- Delete: `scripts/build-mcpb.sh`
- Delete: `packaging/mcpb/manifest.json`
- Delete: `packaging/mcpb/assets/`
- Modify: `README.md`
- Modify: `docs/MCP-SPEC.md`
- Modify: `docs/CLIENT-COMPAT.md`

- [ ] **Step 1: Remove the bundle and active instructions**

Keep runtime MCP icon metadata support. Direct Claude Desktop and other clients through normal Plug.app linking. Historical plans remain unchanged.

- [ ] **Step 2: Verify absence and run tests**

```bash
test ! -e scripts/build-mcpb.sh
test ! -e packaging/mcpb/manifest.json
! rg -n 'build-mcpb|target/dist/plug\.mcpb|install Plug through an MCPB' README.md docs/MCP-SPEC.md docs/CLIENT-COMPAT.md
cargo test --workspace
```

- [ ] **Step 3: Commit**

```bash
git add -A scripts/build-mcpb.sh packaging/mcpb README.md docs/MCP-SPEC.md docs/CLIENT-COMPAT.md
git commit -m "build: retire duplicate MCPB executable"
```

### Task 4: Make standalone artifacts Linux-only

**Files:**
- Modify: `.github/workflows/release.yml`
- Modify: `dist-workspace.toml`
- Modify: `install.sh`
- Delete: `scripts/sign-macos-release.sh`
- Create: `scripts/generate-release-metadata.sh`
- Create: `scripts/test-release-metadata.sh`

**Interfaces:**
- Produces `scripts/generate-release-metadata.sh --version <X.Y.Z> --dmg-sha <sha> --linux-arm-sha <sha> --linux-x64-sha <sha> --output <dir>`.

- [ ] **Step 1: Write metadata contract tests**

Assert Linux Formula URLs only, `depends_on :linux`, app-only Cask, no macOS standalone archive, and Darwin shell-installer refusal with DMG/Cask instructions.

- [ ] **Step 2: Verify failure**

```bash
bash scripts/test-release-metadata.sh
```

- [ ] **Step 3: Implement Linux-only standalone release**

Keep Linux targets in cargo-dist. Keep both Darwin Rust builds only as universal-app inputs. On Darwin, `install.sh` exits with instructions to download Plug.app or run `brew install --cask cyberpapiii/tap/plug-app`.

- [ ] **Step 4: Run and commit**

```bash
bash -n install.sh scripts/generate-release-metadata.sh
bash scripts/test-release-metadata.sh
dist plan --no-local-paths
! rg -n 'apple-darwin' dist-workspace.toml
git add .github/workflows/release.yml dist-workspace.toml install.sh scripts
git commit -m "build: make standalone distribution Linux-only"
```

### Task 5: Generate the app-only Homebrew Cask

**Files:**
- Modify: `scripts/generate-release-metadata.sh`
- Modify: `scripts/test-release-metadata.sh`
- Modify: `.github/workflows/release.yml`

**Generated contract:**

```ruby
cask "plug-app" do
  version "X.Y.Z"
  sha256 "DMG_SHA256"
  url "https://github.com/cyberpapiii/plug/releases/download/v#{version}/Plug-#{version}.dmg"
  auto_updates true
  depends_on macos: ">= :sonoma"
  app "Plug.app"
  uninstall script: {
    executable: "#{appdir}/Plug.app/Contents/Resources/plug",
    args:       ["uninstall-cleanup"],
  }
  caveats "Open Plug once to finish command-line and background-service setup."
end
```

- [ ] **Step 1: Generate the exact contract**

Do not create a Homebrew command link. Plug.app creates `~/.local/bin/plug` on first launch. The uninstall command removes only a symlink proven to target this app and unregisters only the proven app-owned service; unknown files and jobs remain untouched.

- [ ] **Step 2: Audit generated output**

```bash
ruby -c artifacts/plug-app.rb
grep -F 'app "Plug.app"' artifacts/plug-app.rb
grep -F 'auto_updates true' artifacts/plug-app.rb
! grep -E '^[[:space:]]*(binary|postflight)' artifacts/plug-app.rb
grep -F 'uninstall-cleanup' artifacts/plug-app.rb
brew audit --cask --strict artifacts/plug-app.rb
```

- [ ] **Step 3: Commit**

```bash
git add scripts/generate-release-metadata.sh scripts/test-release-metadata.sh .github/workflows/release.yml
git commit -m "build: keep Homebrew installation app-owned"
```

### Task 6: Publish appcast and Cask as one transaction

**Files:**
- Modify: `.github/workflows/release.yml`
- Create: `scripts/publish-release-transaction.sh`

- [ ] **Step 1: Add an idempotent transaction fixture**

Use a disposable prerelease. Force tap push failure and prove GitHub `latest` remains unchanged; retry and prove the tap commit exists before promotion.

- [ ] **Step 2: Implement staged publication**

Jobs become `build-linux`, `build-app`, `prepare-release`, and `publish-release`. The final job creates a prerelease, uploads complete assets, verifies URLs/checksums, pushes one tap commit with Formula+Cask, then promotes the release. Retry reuses the prerelease, uploads with `--clobber`, skips an identical tap commit, and promotes once.

- [ ] **Step 3: Run checks and commit**

```bash
bash -n scripts/publish-release-transaction.sh
shellcheck scripts/publish-release-transaction.sh
actionlint .github/workflows/release.yml
xmllint --noout artifacts/appcast.xml
sha256sum -c artifacts/checksums.sha256
git add .github/workflows/release.yml scripts/publish-release-transaction.sh
git commit -m "build: publish Plug updates as one transaction"
```

### Task 7: Rewrite active installation documentation

**Files:**
- Modify: `README.md`
- Modify: `docs/RELEASING.md`
- Modify: `docs/OPERATOR-GUIDE.md`
- Modify: `docs/USERS.md`
- Modify: `docs/COMPETITIVE.md`
- Modify: `docs/MCP-SPEC.md`
- Modify: `docs/CLIENT-COMPAT.md`
- Modify: `CHANGELOG.md`

- [ ] **Step 1: Rewrite installation order**

Document: macOS website/GitHub DMG; macOS Homebrew Cask; open Plug once; Linux Formula/shell/archive; source development through `plug-dev`. State plainly that headless macOS is unsupported because first-use ServiceManagement and Keychain consent require a logged-in GUI session.

- [ ] **Step 2: Remove active split-install guidance**

```bash
rg -n 'brew install cyberpapiii/tap/plug|cargo install plug-mcp|MCPB|apple-darwin.tar.gz' README.md docs/RELEASING.md docs/OPERATOR-GUIDE.md docs/USERS.md docs/COMPETITIVE.md docs/MCP-SPEC.md docs/CLIENT-COMPAT.md
```

Every remaining match must be explicitly Linux, development, or historical.

- [ ] **Step 3: Commit**

```bash
git add README.md docs CHANGELOG.md
git commit -m "docs: explain one Plug installation on macOS"
```

### Task 8: Release and prove live migration

**Files:**
- Modify after proof: `docs/PROJECT-STATE-SNAPSHOT.md`
- Modify after proof: `docs/PLAN.md`
- Create: user-friendly release notes for the new version

- [ ] **Step 1: Run final source gates**

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
cargo +1.88.0 check --workspace
cargo deny check advisories
scripts/check-todo-status.sh
swift test --package-path PlugApp/PlugIPC
xcodegen generate --spec PlugApp/project.yml
xcodebuild test -project PlugApp/PlugApp.xcodeproj -scheme PlugApp -destination 'platform=macOS'
actionlint .github/workflows/release.yml
```

- [ ] **Step 2: Publish and verify assets**

Require DMG, appcast, app-only Cask, Linux archives/installer/Formula, source, and checksums. Require no macOS CLI archives or MCPB. Verify Developer ID, hardened runtime, notarization/stapling, EdDSA appcast, and Cask checksum.

- [ ] **Step 3: Prove DMG and Cask flows**

Verify first app launch creates the canonical symlink; app, embedded CLI, daemon, shell resolution, and fresh adapter report one version; the recognized Formula is removed through Homebrew; the verified Cargo binary is removed after proof; client configs point into the signed app; unrelated decoys survive.

- [ ] **Step 4: Prove Sparkle and Linux continuity**

Install the prior app, update through Sparkle, and verify automatic daemon/path convergence. Smoke-test the Linux installer in CI.

- [ ] **Step 5: Update truth docs and commit**

Only after live proof, classify unified installation as `done on main` and record the signed/live version and evidence.

```bash
git add docs/PROJECT-STATE-SNAPSHOT.md docs/PLAN.md docs/RELEASE-NOTES-*.md CHANGELOG.md
git commit -m "docs: record unified Plug release proof"
```
