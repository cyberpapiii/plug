# Unified macOS Runtime Ownership Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make every macOS Plug command resolve to one verified executable inside Plug.app, repair known client links conservatively, and expose enough runtime truth for automatic reconciliation.

**Architecture:** A new Rust installation module discovers and verifies the signed Plug.app, delegates stray production binaries into it before command parsing, and supplies the canonical command path to client export and diagnosis. IPC adds backward-compatible adapter and daemon executable evidence; no compatible live connector is killed.

**Tech Stack:** Rust 2024, clap, serde, macOS codesign, launchctl, Unix `exec`, existing Plug IPC and client exporters.

**Spec:** `docs/superpowers/specs/2026-08-25-unified-macos-update-design.md`

## Global Constraints

- Plug.app is the only supported public macOS installation; Linux behavior stays standalone.
- Resolve the app by bundle identifier and verify Developer ID Team ID `HJF7LN64XX`; do not trust a path or label alone.
- `PLUG_DEV=1` is the only delegation bypass.
- Unknown files, client entries, executables, and launchd jobs are reported and never touched.
- Compatible running `plug connect` adapters are not killed.
- Config, OAuth state, credentials, sockets, logs, and Keychain identities are never migrated.
- Every behavior change follows red-green TDD.

---

### Task 1: Discover and verify the canonical Plug.app

**Files:**
- Create: `plug/src/install.rs`
- Modify: `plug/src/main.rs`
- Modify: `plug/Cargo.toml` only if a safe macOS workspace-discovery dependency is required
- Modify: `SECURITY.md`

**Interfaces:**
- Produces:

```rust
pub const APP_BUNDLE_ID: &str = "com.cyberpapiii.plug";
pub const DEVELOPER_TEAM_ID: &str = "HJF7LN64XX";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedAppInstallation {
    pub bundle_path: PathBuf,
    pub executable_path: PathBuf,
    pub version: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DelegationDecision { Stay, Exec(PathBuf) }

pub fn resolve_verified_app() -> anyhow::Result<Option<VerifiedAppInstallation>>;
pub fn canonical_client_command() -> anyhow::Result<PathBuf>;
pub fn delegation_decision(current: &Path, app: Option<&VerifiedAppInstallation>, dev: bool)
    -> anyhow::Result<DelegationDecision>;
pub fn maybe_delegate_to_app() -> anyhow::Result<()>;
```

- [ ] **Step 1: Write failing pure decision tests**

Add inline tests proving: absent app stays; the bundle executable stays; a stray executable delegates; `PLUG_DEV=1` stays; an invalid signature/version returns an error; moved app paths are accepted; a loop marker cannot cause a second delegation.

- [ ] **Step 2: Run the focused tests and confirm failure**

```bash
cargo test -p plug-mcp install::tests -- --nocapture
```

Expected: compilation fails because `install` and the interfaces do not exist.

- [ ] **Step 3: Implement verified discovery and delegation**

Locate `com.cyberpapiii.plug` through a safe macOS API wrapper, falling back only to `/Applications/Plug.app` and `~/Applications/Plug.app`. Require `Contents/Resources/plug`, matching bundle/embedded versions, and successful `codesign --verify --strict` plus a designated requirement containing `anchor apple generic`, identifier `com.cyberpapiii.plug`, and Team ID `HJF7LN64XX`.

`maybe_delegate_to_app` runs before dotenv loading and clap parsing, forwards `args_os`, cwd, environment, and stdio, sets a private loop marker, and calls `std::os::unix::process::CommandExt::exec`. It returns normally only for `Stay`.

Document the accepted local TOCTOU boundary in `SECURITY.md`: a same-user attacker able to replace a verified application between signature verification and `exec` already controls the user's application files; Plug still verifies immediately before delegation and never delegates to an unsigned or wrong-Team-ID target.

- [ ] **Step 4: Run focused and binary tests**

```bash
cargo test -p plug-mcp install::tests
cargo test -p plug-mcp --bin plug
```

- [ ] **Step 5: Commit**

```bash
git add plug/src/install.rs plug/src/main.rs plug/Cargo.toml Cargo.lock SECURITY.md
git commit -m "feat: delegate macOS commands to verified Plug app"
```

### Task 2: Canonicalize client exports and conservative repair

**Files:**
- Modify: `plug/src/commands/clients.rs`
- Modify: `plug/src/commands/misc.rs`
- Test: inline tests in both files

**Interfaces:**
- Consumes: `install::canonical_client_command()`
- Produces:

```rust
#[derive(Debug, Clone, Serialize)]
pub enum PlugLinkDisposition { Canonical, RecognizedLegacy, UnknownCommand, Http, Missing }

#[derive(Debug, Clone, Serialize)]
pub struct ClientRepairItem {
    pub target: String,
    pub path: PathBuf,
    pub disposition: PlugLinkDisposition,
    pub changed: bool,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ClientRepairReport {
    pub canonical_command: PathBuf,
    pub items: Vec<ClientRepairItem>,
}

pub fn classify_plug_client_command(
    command: &str,
    args: &[String],
    canonical: &Path,
) -> PlugLinkDisposition;
```

- [ ] **Step 1: Write preservation and classification regressions**

Cover JSON clients, Codex TOML, and Goose YAML. Prove canonical entries are unchanged; recognized Cargo, Homebrew, old app, and repository `target/*/plug connect` entries repair; unrelated commands and project-local entries remain byte-for-byte untouched; unrelated servers and unknown fields survive.

- [ ] **Step 2: Verify the tests fail**

```bash
cargo test -p plug-mcp commands::clients::tests
cargo test -p plug-mcp commands::misc::tests
```

- [ ] **Step 3: Implement canonical export and JSON repair reporting**

Remove the `current_exe()` lookup from `execute_export`; resolve once through `canonical_client_command`. Extend linked-config parsing to retain command and args. Recognize a legacy entry only when its resolved executable identifies as Plug and its invocation is `connect`. Make `plug repair --all --output json` serialize `ClientRepairReport`; text output remains calm and unchanged for healthy state.

- [ ] **Step 4: Run targeted and workspace tests**

```bash
cargo test -p plug-mcp commands::clients::tests
cargo test -p plug-mcp commands::misc::tests
cargo test --workspace
```

- [ ] **Step 5: Commit**

```bash
git add plug/src/commands/clients.rs plug/src/commands/misc.rs
git commit -m "feat: repair clients to the app-owned Plug command"
```

### Task 3: Add backward-compatible adapter version telemetry

**Files:**
- Modify: `plug-core/src/ipc.rs`
- Modify: `plug/src/runtime.rs`
- Modify: `plug/src/daemon/registry.rs`
- Modify: `plug/src/daemon/mod.rs`
- Modify fixtures in: `plug/src/ipc_proxy.rs`, `plug/src/views/overview.rs`

**Interfaces:**

```rust
IpcRequest::Register {
    protocol_version: u16,
    client_id: String,
    client_info: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    adapter_version: Option<String>,
}
```

Add `#[serde(default)] pub adapter_version: Option<String>` to stored and live session records.

- [ ] **Step 1: Write compatibility tests**

Decode old Register JSON without `adapter_version` as `None`, round-trip new JSON, and prove registry/list output preserves `Some("0.6.5")`.

- [ ] **Step 2: Verify failure**

```bash
cargo test -p plug-core ipc::tests
cargo test -p plug-mcp daemon::registry::tests
```

- [ ] **Step 3: Implement telemetry**

Send `Some(env!("CARGO_PKG_VERSION").into())` from every daemon-proxy registration. Store and expose it for daemon-proxy sessions; HTTP/SSE sessions use `None`. Do not add exact-version rejection.

- [ ] **Step 4: Run all affected tests**

```bash
cargo test -p plug-core ipc::tests
cargo test -p plug-mcp daemon::registry::tests
cargo test -p plug-mcp ipc_proxy
cargo test --workspace
```

- [ ] **Step 5: Commit**

```bash
git add plug-core/src/ipc.rs plug/src/runtime.rs plug/src/daemon/registry.rs plug/src/daemon/mod.rs plug/src/ipc_proxy.rs plug/src/views/overview.rs
git commit -m "feat: report connector binary versions"
```

### Task 4: Report the daemon executable and classify launchd by evidence

**Files:**
- Modify: `plug-core/src/ipc.rs`
- Modify: `plug/src/daemon/mod.rs`
- Modify: `plug/src/service.rs`

**Interfaces:**

```rust
pub struct OperatorHandshake {
    // existing fields
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daemon_executable: Option<PathBuf>,
}

pub struct LaunchdJobRecord { pub label: String, pub program: PathBuf }
pub enum LaunchdProgramOwnership { CurrentApp, RecognizedLegacyPlug, Unknown }
```

- [ ] **Step 1: Write old/new IPC and launchd classification tests**

Prove absent `daemon_executable` decodes, current daemon reports a canonical path, alternate labels targeting verified Plug classify as recognized, and `com.plug.daemon` targeting unrelated software remains unknown.

- [ ] **Step 2: Verify failure**

```bash
cargo test -p plug-core ipc::tests
cargo test -p plug-mcp service::tests
```

- [ ] **Step 3: Implement optional evidence**

Populate the handshake from canonicalized `current_exe`. Enumerate the user launchd domain for diagnosis, resolve program paths, and classify ownership from path plus Plug identity. Preserve fixed-label startup authority; broad discovery is diagnostic only.

- [ ] **Step 4: Run tests and commit**

```bash
cargo test -p plug-core ipc::tests
cargo test -p plug-mcp service::tests
cargo test --workspace
git add plug-core/src/ipc.rs plug/src/daemon/mod.rs plug/src/service.rs
git commit -m "feat: expose daemon and launchd ownership evidence"
```

### Task 5: Add unified installation diagnosis

**Files:**
- Modify: `plug/src/install.rs`
- Modify: `plug/src/commands/misc.rs`
- Modify: `plug/src/main.rs` only if an internal JSON inspection command is required

**Interfaces:**

```rust
#[derive(Debug, Serialize)]
pub struct UnifiedInstallSnapshot {
    pub app: Option<VerifiedAppInstallation>,
    pub shell_resolution: Option<PathBuf>,
    pub daemon_version: Option<String>,
    pub daemon_executable: Option<PathBuf>,
    pub ownership: DaemonOwnershipMode,
    pub linked_clients: Vec<ClientRepairItem>,
    pub adapters: Vec<AdapterVersionState>,
    pub shadows: Vec<ShadowInstallation>,
    pub launchd_jobs: Vec<LaunchdJobRecord>,
}

#[derive(Debug, Serialize)]
pub enum AdapterVersionState { Current, CompatibleOlder, Missing, Incompatible }

#[derive(Debug, Serialize)]
pub struct ShadowInstallation {
    pub kind: String,
    pub path: PathBuf,
    pub verified_plug_owned: bool,
}
```

- [ ] **Step 1: Write the complete diagnosis matrix**

Fixtures cover healthy, absent app, wrong shell resolution, stale daemon version/path, CLI-owned daemon, stale client links, recognized Cargo/Formula shadows, missing/older/current adapter versions, and unknown files/jobs. Pin the healthy message exactly: `Plug.app owns the app, command line, daemon, and client links.`

- [ ] **Step 2: Verify failure**

```bash
cargo test -p plug-mcp commands::misc::tests -- unified_install
```

- [ ] **Step 3: Implement bounded inspection**

Resolve the actual command using the user's login shell (`$SHELL -lic 'command -v plug'`) with a timeout. Request operator handshake and live sessions. Verify identity before calling any shadow Plug-owned. Return one repair action: open Plug.app and retry reconciliation. Missing or older compatible adapters are informational only.

Add the hidden `plug uninstall-cleanup` command used by the Cask. It unregisters only a launchd service proven app-owned and removes `~/.local/bin/plug` only when the resolved symlink target is the verified current app executable. An unrelated file, unknown job, or failed proof is reported and left untouched.

- [ ] **Step 4: Run gates and commit**

```bash
cargo test -p plug-mcp commands::misc::tests
cargo test -p plug-mcp install::tests -- uninstall_cleanup
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
git add plug/src/install.rs plug/src/commands/misc.rs plug/src/main.rs
git commit -m "feat: diagnose unified macOS installation ownership"
```

### Task 6: Prove the Rust contract for the app coordinator

**Files:**
- Modify tests in the files above
- Modify: `docs/solutions/` only if implementation uncovers a reusable failure pattern

**Interfaces:**
- Produces the stable app-facing commands:

```text
plug repair --all --output json
plug doctor --output json
```

- [ ] **Step 1: Run release-mode behavior tests with a fake signed-app fixture**

Exercise canonical export, full delegation planning, old adapter decoding, daemon executable reporting, unknown-item preservation, and healthy doctor output in one integration test.

- [ ] **Step 2: Run the full repository gate**

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
cargo +1.88.0 check --workspace
cargo deny check advisories
scripts/check-todo-status.sh
```

- [ ] **Step 3: Commit any fixture-only additions**

```bash
git add plug plug-core tests docs/solutions
git commit -m "test: prove app-owned macOS runtime contract"
```
