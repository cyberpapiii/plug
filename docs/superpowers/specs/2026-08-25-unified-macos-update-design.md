# Unified macOS Installation and Update Design

**Date:** 2026-08-25

**Status:** Approved

**Scope:** macOS public installation, updates, daemon ownership, command-line access, client configuration, migration, diagnosis, and release packaging

## Purpose

Plug should feel like one product. A macOS user installs Plug once and updates Plug once. The app, command-line interface, daemon, and client configuration must not acquire separate binary owners or drift between versions.

The current release has one universal `plug` binary embedded in Plug.app, but it also publishes and documents independent Cargo, Homebrew Formula, shell-installer, manual archive, and development installs. Sparkle updates Plug.app while those other copies remain unchanged. Client linking makes the problem durable by writing the path of whichever executable happened to run `plug link` or `plug repair`.

This design removes split ownership instead of adding synchronization among copies.

## Product Contract

On macOS:

- Plug.app is the only supported public installation.
- The signed Plug.app located by bundle identifier is the only public installation, normally at `/Applications/Plug.app` and optionally at `~/Applications/Plug.app`.
- The app bundle's `Contents/Resources/plug` is the only installed executable.
- The app-owned launchd service runs that executable.
- Shell commands and MCP client configurations reference that executable, directly or through a symlink.
- Sparkle replaces Plug.app, so it updates the app, daemon source, and command-line target in one transaction.
- Startup reconciliation finishes an update automatically and proves the new daemon is running before reporting success.
- Existing compatible `plug connect` processes may remain alive until their host naturally reconnects; they are transient sessions, not separately installed products.
- Failures produce one clear state and one repair action. Reconciliation is bounded and never loops indefinitely.

Linux remains a standalone CLI product. Source builds on macOS remain available for development but are not a public installation channel.

## Chosen Architecture

### One executable owner

Canonical executable, resolved from the signed bundle rather than hardcoded:

```text
/Applications/Plug.app/Contents/Resources/plug
```

Plug.app already bundles a universal signed executable and its SMAppService LaunchAgent already uses the bundle-relative executable. That mechanism becomes the sole macOS authority. Installation discovery resolves `com.cyberpapiii.plug` through macOS workspace services, accepts `/Applications` and `~/Applications`, and verifies the Developer ID Team ID `HJF7LN64XX`. A moved app is repaired on its next launch. Sparkle may warn that `/Applications` is preferred, but Plug does not reject a valid user-local installation.

No production workflow copies `plug` into `~/.cargo/bin`, `~/.local/bin`, `/opt/homebrew/bin`, or an MCP bundle. Command-line locations contain symlinks only.

### Shell command

Direct DMG installations maintain:

```text
~/.local/bin/plug -> /Applications/Plug.app/Contents/Resources/plug
```

The Homebrew Cask installs Plug.app only. It has no `binary` or `postflight` symlink stanza, so Homebrew never becomes a second command-path writer. Opening Plug once finishes setup, including the shell symlink, daemon consent, and legacy adoption.

The app repairs the user-owned `~/.local/bin/plug` symlink atomically on every launch. It never overwrites an unrelated regular file. A conflicting unrelated file produces one actionable repair message.

Any standalone macOS `plug` process that is not running from the verified bundle fully re-executes the bundle executable before dispatching any command when a signed Plug.app is present. It does not maintain a matrix of delegated and local commands. Delegation verifies the target signature and Team ID, refuses an invalid target, and uses a loop guard plus a self-path check. `PLUG_DEV=1` is the sole explicit escape for source-development builds. Unknown executables and unrelated files are reported and never touched.

### MCP client configuration

On macOS, `plug link`, `plug repair`, and generated stdio client entries use the currently resolved signed app executable path, not `std::env::current_exe()` and not PATH lookup. A normal `/Applications` installation therefore writes:

```text
/Applications/Plug.app/Contents/Resources/plug connect
```

This keeps Claude, Codex, Cursor, OpenCode, and other clients attached to the updated app even when an agent launches `plug` through a legacy path.

When Plug.app is not installed, source-development builds retain current-executable behavior and identify themselves as development mode. They do not silently replace the public command-line symlink. Unknown client entries and unrelated MCP servers are reported and never touched.

### Daemon ownership

Existing `com.plug.daemon` SMAppService ownership, startup lock, and app-wins classification remain authoritative.

Normal app startup runs one idempotent reconciliation:

1. Verify app version matches embedded `plug --version`.
2. Repair the user command symlink.
3. Repair recognized Plug client entries to the canonical app path.
4. Enumerate launchd jobs broadly, but classify ownership from each job's resolved program path rather than its label. Only jobs targeting the signed bundle or recognized legacy Plug binaries are eligible for adoption or removal.
5. Connect to the daemon and compare its runtime version with the app version.
6. If app-managed daemon is stale, use the existing safe replacement flow: pause adapters, unregister/register the service, stop the old daemon, kickstart the new service, and wait for an exact-version handshake.
7. Resume adapters and refresh operator state.
8. Report success only after launchd and daemon proof agree.

Unknown launchd jobs, labels, programs, and files are reported and never touched.

First adoption remains a user-visible action because macOS may require Login Items consent. Routine app-owned upgrades reconcile automatically.

### Running adapters

`plug connect` processes are transient client sessions. They reconnect and replay state when the daemon restarts. Automatically killing every adapter is unsafe because MCP hosts do not share a reliable respawn contract; doing so could turn a successful update into a client outage.

Registration gains an optional adapter binary version. The daemon stores and exposes it in live client state. Compatible older adapters continue until their host naturally reconnects. Incompatible adapters fail clearly with the affected client named. No global client restart is required for a normal compatible update.

The first migration handles adapters that predate version reporting by pausing them during daemon replacement, then resuming them. New sessions start from the canonical app path.

## Installation Coordinator

One coordinator owns reconciliation. Swift orchestrates macOS service operations and invokes Rust-owned client repair rather than editing every client format independently.

Inputs:

- Plug.app path and bundle version
- embedded executable path and version
- SMAppService status and launchd record
- daemon handshake version and ownership
- `~/.local/bin/plug` state
- known shadow installations
- linked client command paths
- live adapter versions when available

States:

- `healthy`
- `adoptionRequired`
- `reconcilingUpdate`
- `repairableDrift`
- `blocked`

Normal healthy state remains silent. Reconciliation shows only “Finishing Plug update…” when visible long enough to matter.

Blocked state uses one sentence plus `Retry` and `View Log`. It preserves the underlying error for diagnostics. It never presents separate app, CLI, daemon, and client repair choices.

## Migration

Migration is idempotent and conservative.

### Recognized legacy state

- `~/.local/bin/plug` symlink to `~/.cargo/bin/plug`
- Plug-owned regular executable at `~/.cargo/bin/plug`
- Homebrew Formula `cyberpapiii/tap/plug`
- client entries pointing to recognized Cargo, Homebrew, old app, or repository build paths
- CLI-owned `~/Library/LaunchAgents/com.plug.daemon.plist`
- other launchd jobs whose resolved program path proves they target a recognized Plug binary, regardless of label

### Migration behavior

- Atomically replace `~/.local/bin/plug` with the canonical app symlink.
- Delete a recognized Plug executable in `~/.cargo/bin` only after the signed app, embedded executable, repaired shell path, and app-owned daemon have all been verified. Unknown binaries and unrelated files are reported and never touched.
- If the exact Homebrew Formula `cyberpapiii/tap/plug` is installed, locate Homebrew only at its standard trusted paths and run `brew uninstall cyberpapiii/tap/plug`; never delete keg files or alter Homebrew's database directly. Formula removal precedes any command-path repair. If Homebrew removal fails, leave the Formula dormant and show the exact repair command. Unknown formulae, casks, taps, and package files are reported and never touched.
- Repair only known Plug entries in detected client files, preserving unknown fields, comments where the format permits, project-local settings, and unrelated MCP servers.
- Remove legacy launchd jobs only through existing adoption logic and only when their resolved program path proves Plug ownership. Unknown jobs and unrelated files are reported and never touched.
- Preserve config, OAuth state, tokens, sockets, logs, and Keychain identities.

An unrelated file at any target path stops that repair step and surfaces a precise message. Unknown files are reported and never touched. No blind overwrite, manual package-manager mutation, or credential migration occurs.

## Update Flow

Sparkle remains the sole in-place macOS updater.

1. Sparkle verifies EdDSA and Developer ID signatures.
2. Sparkle replaces Plug.app and relaunches it.
3. New app startup runs installation reconciliation.
4. Canonical symlinks and client entries now reference the replaced bundle.
5. Stale app-owned daemon is restarted once through SMAppService.
6. New daemon handshake must report the new version.
7. App returns to normal operator state.

No pre-install daemon shutdown is required. If update replacement fails, old signed app and daemon remain intact. If post-update daemon restart fails, new app remains usable enough to show `Retry` and `View Log`; bounded retry prevents thrashing.

## Release Packaging

### macOS

Public artifacts:

- signed, notarized, stapled Plug DMG
- signed Sparkle appcast
- Homebrew Cask using the same DMG, installing only Plug.app, and instructing the user to launch Plug once to finish setup
- checksums and source archive

Removed as macOS public installation paths:

- standalone Homebrew Formula binary
- cargo-dist macOS installer
- standalone macOS CLI archives as recommended installs
- package-local MCPB executable
- `cargo install` instructions for normal users

The Homebrew Formula and shell installer remain for Linux. The crates remain publishable for development and downstream builds.

MCPB distribution is retired unless the format can reference the resolved app executable without embedding another copy. A package-local executable violates the one-owner contract.

The release job publishes the DMG, Sparkle appcast, and Cask update as one release operation. It does not update the appcast and Cask independently, preventing `brew upgrade --greedy` from reinstalling a Cask version older than the current Sparkle release.

### Version source

Workspace package version is the release source. Build and release gates verify:

- release tag equals Cargo workspace version
- `CFBundleShortVersionString` equals workspace version
- embedded `plug --version` equals app version
- Sparkle appcast version equals app version
- Cask version equals app version
- app build number increases monotonically

Swift handshake metadata reads `Bundle.main`; it contains no hardcoded release version.

## Diagnostics and Repair

`plug doctor` gains one `unified_install` check on macOS. It reports:

- canonical app and embedded executable
- shell command resolution
- daemon executable, ownership, and version
- linked-client command conformity
- live adapter versions when available
- dormant shadow installations
- app bundle location and Developer ID Team ID

Healthy output is one line: “Plug.app owns the app, command line, daemon, and client links.”

Repair suggestion is one action: open Plug.app and run reconciliation. `plug repair` invoked from the canonical executable may run the Rust-owned client and symlink repair directly. Doctor does not ask users to coordinate Sparkle, Cargo, Homebrew, launchd, and individual config files.

Existing doctor warnings remain separate:

- plaintext upstream OAuth files are protected with mode `0600`; Keychain migration is security hardening, not part of update unification
- client tool limits depend on lazy-discovery policy; policy-aware warning refinement is follow-up work
- public OAuth reachability remains independently verified

## Development Workflow

`scripts/dev-reinstall.sh` no longer takes over public `plug` paths by default. It installs `plug-dev` or runs a repository build explicitly with `PLUG_DEV=1`. An `--activate` option may temporarily point the development environment at that build, with a matching restore command and a visible warning.

Development builds never modify Plug.app, Sparkle state, Homebrew state, or production client configuration without explicit activation.

## Testing

### Rust

- canonical path selection with and without Plug.app
- bundle discovery in `/Applications`, `~/Applications`, and moved-app scenarios
- delegation target signature and Team ID verification, full-exec behavior, and loop prevention
- `PLUG_DEV=1` delegation bypass
- client export always uses canonical path in production macOS mode
- config migration for every supported JSON, TOML, and YAML client format
- unknown fields and unrelated servers survive migration
- optional adapter version decodes from old and new clients
- doctor state matrix for healthy, stale CLI, stale daemon, stale client links, and shadow installs
- doctor checks actual shell resolution order, not merely expected symlink targets
- launchd discovery classifies ownership by resolved program path and preserves unknown jobs
- CLI-owned startup fallback remains valid outside app-managed production mode

Every behavior change follows red-green TDD.

### Swift

- reconciliation-state matrix
- symlink creation, repair, broken-link handling, and unrelated-file refusal
- first adoption remains explicit
- stale app-managed daemon automatically uses verified replacement
- daemon restart succeeds only after exact-version handshake
- bounded failure produces one stable error state
- bundle version drives handshake metadata

### Integration and release

- clean DMG install creates canonical shell access and app-owned daemon after first launch
- clean Cask install claims no command path and converges after first app launch
- migration fixture covers Cargo, Formula removal through Homebrew, old client entries, known-path launchd jobs, and unknown-item preservation
- Sparkle update fixture proves app, embedded CLI, daemon, and client paths converge
- real stdio adapter survives daemon replacement and replays session state
- Cask and Sparkle metadata are published from the same release transaction
- installed app, daemon, shell command, and fresh adapter report the same version
- Developer ID, notarization, stapling, appcast signatures, checksums, full Rust gates, Swift tests, Xcode build, MSRV, clippy, formatting, dependency advisories, and todo guard remain release gates

## Rollout

1. Land diagnosis, canonical-path selection, client repair, adapter-version telemetry, and tests without changing packaging.
2. Land app reconciliation and migration with fixture-based tests.
3. Change macOS packaging and documentation to app-only ownership.
4. Publish a signed release.
5. Install on the current machine and migrate the existing Cargo/Homebrew split.
6. Verify app, shell CLI, daemon, launchd, client links, fresh adapters, all enabled upstreams, public OAuth metadata, and one real routed tool call.
7. Update `docs/PROJECT-STATE-SNAPSHOT.md` and `docs/PLAN.md` from verified live state.
8. Confirm no recognized legacy macOS executable, Formula, command path, client entry, or launchd job remains; unknown files and jobs remain untouched.

## Success Criteria

- One macOS executable exists in the supported installation.
- Sparkle or Homebrew Cask update replaces that executable once.
- App, daemon, shell CLI, and client configurations resolve to it.
- Normal updates require no manual daemon, CLI, or client repair.
- Update failure leaves a working previous daemon or one bounded, actionable repair state.
- Doctor explains state without exposing installation internals during normal operation.
- Linux and source-development workflows remain functional without becoming macOS production owners.
