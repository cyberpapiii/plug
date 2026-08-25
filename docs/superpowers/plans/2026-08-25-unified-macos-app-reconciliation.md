# Unified macOS App Reconciliation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make Plug.app silently converge the app, command, daemon, client links, and recognized legacy installations after first launch and every update.

**Architecture:** A testable installation coordinator composes signed-app inspection, bounded process execution, conservative migration, launchd evidence, Rust-owned client repair, and exact-version daemon replacement. First adoption remains explicit; routine app-owned updates reconcile automatically.

**Tech Stack:** Swift 6, SwiftUI, Observation, ServiceManagement, Security framework, Sparkle 2, PlugIPC, XCTest.

**Spec:** `docs/superpowers/specs/2026-08-25-unified-macos-update-design.md`

## Global Constraints

- macOS 14+ and direct Developer ID distribution.
- The app bundle is discovered dynamically and must verify Team ID `HJF7LN64XX`.
- The app is the sole writer of `~/.local/bin/plug`; Homebrew has no symlink hook.
- Unknown files, jobs, packages, and client entries are reported and never touched.
- First SMAppService consent is explicit; app-owned version convergence is automatic.
- Reconciliation is single-flight, bounded, and never loops.
- Success requires exact app, embedded binary, launchd, and daemon version proof.

---

### Task 1: Add installation domain types and bounded process execution

**Files:**
- Create: `PlugApp/PlugApp/Models/InstallationState.swift`
- Create: `PlugApp/PlugApp/Services/ProcessRunner.swift`
- Create: `PlugApp/PlugAppTests/ProcessRunnerTests.swift`

**Interfaces:**

```swift
enum InstallationState: Equatable {
    case healthy(InstallationSnapshot)
    case adoptionRequired(InstallationSnapshot)
    case reconcilingUpdate(ReconciliationPhase)
    case repairableDrift(InstallationDrift)
    case blocked(InstallationFailure)
}

enum ReconciliationPhase: Equatable {
    case inspecting
    case removingLegacyFormula
    case repairingCommand
    case repairingClients
    case replacingDaemon
    case verifying
    case cleaningLegacyBinary
}

enum ShellLinkState: Equatable, Sendable {
    case absent
    case canonical(URL)
    case repairable(URL?)
    case unrelated(URL)
}

struct ShadowInstall: Equatable, Sendable {
    enum Kind: String, Sendable { case cargo, homebrewFormula, clientLink, launchdJob }
    let kind: Kind
    let url: URL
}

struct InstallationSnapshot: Equatable, Sendable {
    let app: VerifiedAppInstallation
    let shellLink: ShellLinkState
    let service: DaemonServiceSnapshot
    let daemonVersion: String?
    let clientRepairNeeded: Bool
    let shadowInstalls: [ShadowInstall]
}

struct InstallationDrift: Equatable, Sendable {
    let summary: String
    let detail: String
}

struct InstallationFailure: Equatable, Sendable {
    let summary: String
    let detail: String
    let logURL: URL?
}

struct ProcessResult: Sendable {
    let status: Int32
    let stdout: Data
    let stderr: Data
}

protocol ProcessRunning: Sendable {
    func run(executable: URL, arguments: [String], timeout: Duration) async throws -> ProcessResult
}

```

- [ ] **Step 1: Write failing runner tests**

Prove stdout/stderr capture, nonzero status preservation, timeout termination, and no blocking of `MainActor`.

- [ ] **Step 2: Verify failure**

```bash
xcodegen generate --spec PlugApp/project.yml
xcodebuild test -project PlugApp/PlugApp.xcodeproj -scheme PlugApp -destination 'platform=macOS' -only-testing:PlugAppTests/ProcessRunnerTests
```

- [ ] **Step 3: Implement the state model and runner**

Use one detached Foundation `Process`, concurrent pipe draining, a timeout task, termination on expiry, and exactly one continuation resume. Associated phase values remain details of `reconcilingUpdate`, not extra product states.

- [ ] **Step 4: Run and commit**

```bash
xcodebuild test -project PlugApp/PlugApp.xcodeproj -scheme PlugApp -destination 'platform=macOS' -only-testing:PlugAppTests/ProcessRunnerTests
git add PlugApp
git commit -m "feat: add bounded installation process runner"
```

### Task 2: Verify the running app and inspect legacy state

**Files:**
- Create: `PlugApp/PlugApp/Services/AppInstallationInspector.swift`
- Create: `PlugApp/PlugApp/Services/LegacyInstallMigrator.swift`
- Create: `PlugApp/PlugApp/Services/LaunchdJobInspector.swift`
- Create: `PlugApp/PlugAppTests/AppInstallationInspectorTests.swift`
- Create: `PlugApp/PlugAppTests/LegacyInstallMigratorTests.swift`
- Create: `PlugApp/PlugAppTests/LaunchdJobInspectorTests.swift`
- Modify: `PlugApp/project.yml` if explicit Security framework linkage is required

**Interfaces:**

```swift
struct VerifiedAppInstallation: Equatable, Sendable {
    let bundleURL: URL
    let executableURL: URL
    let appVersion: String
    let buildVersion: String
    let embeddedVersion: String
    let teamID: String
}

struct ReconciliationProof: Sendable {
    let appVersion: String
    let embeddedVersion: String
    let daemonVersion: String
    let shellTarget: URL
    let appManaged: Bool
}

struct LegacyInstallSnapshot: Equatable, Sendable {
    let formulaInstalled: Bool
    let cargoBinary: URL?
    let shellLink: ShellLinkState
    let recognizedPaths: Set<URL>
    let unknownPaths: Set<URL>
}

struct DaemonServiceSnapshot: Equatable, Sendable {
    let ownership: DaemonOwnershipState
    let daemonVersion: String?
    let daemonExecutable: URL?
}

struct LaunchdJobRecord: Equatable, Sendable {
    let label: String
    let programURL: URL?
    let parentBundleIdentifier: String?
    let parentBundleVersion: String?
    let loaded: Bool
}

enum DaemonOwnershipState: Equatable, Sendable {
    case appManagedCurrent(LaunchdJobRecord)
    case appManagedStale(LaunchdJobRecord)
    case recognizedLegacy([LaunchdJobRecord])
    case unmanaged
    case unknown([LaunchdJobRecord])
}

protocol AppInstallationInspecting: Sendable {
    func inspectCurrentApp() async throws -> VerifiedAppInstallation
}

protocol LaunchdJobInspecting: Sendable {
    func daemonJobs(
        canonical: VerifiedAppInstallation,
        recognizedLegacyPaths: Set<URL>
    ) async throws -> DaemonOwnershipState
}

protocol LegacyInstallMigrating: Sendable {
    func inspect(canonical: VerifiedAppInstallation) async throws -> LegacyInstallSnapshot
    func removeRecognizedFormula(_ snapshot: LegacyInstallSnapshot) async throws
    func repairShellLink(to executable: URL) async throws -> ShellLinkState
    func removeVerifiedCargoBinary(_ snapshot: LegacyInstallSnapshot, proof: ReconciliationProof) async throws
}
```

- [ ] **Step 1: Write signature, migration, and launchd fixtures**

Cover `/Applications`, `~/Applications`, moved valid app, wrong Team ID, app/binary version mismatch, absent/broken/canonical shell links, unrelated regular-file refusal, exact Formula removal before link repair, failed brew preservation, Cargo retention before proof and deletion after proof, alternate-label known Plug job, exact-label unrelated job, and unknown-job preservation.

- [ ] **Step 2: Verify failure**

```bash
xcodebuild test -project PlugApp/PlugApp.xcodeproj -scheme PlugApp -destination 'platform=macOS' \
  -only-testing:PlugAppTests/AppInstallationInspectorTests \
  -only-testing:PlugAppTests/LegacyInstallMigratorTests \
  -only-testing:PlugAppTests/LaunchdJobInspectorTests
```

- [ ] **Step 3: Implement evidence-based inspection and migration**

Use Security framework static-code validation for the current bundle. Inspect Homebrew only at `/opt/homebrew/bin/brew` and `/usr/local/bin/brew`; invoke `brew uninstall cyberpapiii/tap/plug`, never mutate keg files. Create the shell link through a same-directory temporary symlink plus atomic rename. Enumerate user launchd jobs, resolve their program paths, and authorize mutation only for the signed bundle or recognized legacy Plug binaries.

- [ ] **Step 4: Run tests and commit**

```bash
xcodegen generate --spec PlugApp/project.yml
xcodebuild test -project PlugApp/PlugApp.xcodeproj -scheme PlugApp -destination 'platform=macOS'
git add PlugApp
git commit -m "feat: inspect and migrate verified Plug installations"
```

### Task 3: Make daemon replacement exact and evidence-backed

**Files:**
- Modify: `PlugApp/PlugApp/Services/DaemonServiceManager.swift`
- Create: `PlugApp/PlugAppTests/DaemonServiceManagerTests.swift`
- Modify: `PlugApp/PlugAppTests/AppModelTests.swift`

**Interfaces:**

```swift
func inspect(canonical: VerifiedAppInstallation, legacyPaths: Set<URL>) async throws -> DaemonServiceSnapshot
func adoptRecognizedLegacy(snapshot: DaemonServiceSnapshot, expectedVersion: String) async throws -> OperatorHandshake
func replaceStaleAppService(snapshot: DaemonServiceSnapshot, expectedVersion: String) async throws -> OperatorHandshake
func ensureRunning(expectedVersion: String) async throws -> OperatorHandshake
```

- [ ] **Step 1: Move and expand service tests**

Pin explicit first adoption, automatic stale app replacement, connector pause before bootout, connector resume on success and failure, exact-version handshake, wrong-version ready socket failure, bounded retries, and refusal to boot out unknown jobs.

- [ ] **Step 2: Verify failure**

```bash
xcodebuild test -project PlugApp/PlugApp.xcodeproj -scheme PlugApp -destination 'platform=macOS' -only-testing:PlugAppTests/DaemonServiceManagerTests
```

- [ ] **Step 3: Consolidate replacement mechanics**

Replace `needsAdoption`, unverified `restart()`, and exact-label cleanup with one private replacement primitive consuming verified job records. Preserve pause/resume with `defer`. Success requires launchd program path, parent bundle ID/build, and operator handshake `daemonVersion` to match the current verified app.

- [ ] **Step 4: Run tests and commit**

```bash
xcodebuild test -project PlugApp/PlugApp.xcodeproj -scheme PlugApp -destination 'platform=macOS'
git add PlugApp
git commit -m "feat: prove app-owned daemon replacement"
```

### Task 4: Invoke Rust-owned client repair

**Files:**
- Create: `PlugApp/PlugApp/Services/ClientRepairService.swift`
- Create: `PlugApp/PlugAppTests/ClientRepairServiceTests.swift`

**Interfaces:**

```swift
struct ClientRepairResult: Codable, Equatable, Sendable {
    let examined: Int
    let repaired: Int
    let unchanged: Int
}

protocol ClientRepairing: Sendable {
    func inspect(canonicalExecutable: URL) async throws -> Bool
    func repairAll(canonicalExecutable: URL) async throws -> ClientRepairResult
}
```

- [ ] **Step 1: Write fake-command tests**

Prove drift/no-drift inspection, canonical executable invocation, valid JSON decoding, nonzero stderr propagation, malformed JSON blocking, and no Swift-side client-file edits.

- [ ] **Step 2: Verify failure**

```bash
xcodebuild test -project PlugApp/PlugApp.xcodeproj -scheme PlugApp -destination 'platform=macOS' -only-testing:PlugAppTests/ClientRepairServiceTests
```

- [ ] **Step 3: Implement the wrapper**

Execute the verified embedded binary as `plug repair --all --output json`; decode the stable Rust report and return it. No alternate command path is allowed.

- [ ] **Step 4: Run and commit**

```bash
xcodebuild test -project PlugApp/PlugApp.xcodeproj -scheme PlugApp -destination 'platform=macOS' -only-testing:PlugAppTests/ClientRepairServiceTests
git add PlugApp
git commit -m "feat: connect app reconciliation to client repair"
```

### Task 5: Build the single-flight installation coordinator

**Files:**
- Create: `PlugApp/PlugApp/Services/InstallationCoordinator.swift`
- Create: `PlugApp/PlugAppTests/InstallationCoordinatorTests.swift`

**Interfaces:**

```swift
enum ReconciliationTrigger { case applicationLaunch, retry, explicitAdoption }

@MainActor @Observable
final class InstallationCoordinator {
    private(set) var state: InstallationState
    func reconcile(trigger: ReconciliationTrigger) async
    func adopt() async
    func retry() async
    func openLog()
}
```

- [ ] **Step 1: Write the orchestration matrix**

Use protocol fakes to pin exact order: verify app; inspect legacy; uninstall Formula; repair shell link; repair clients; inspect launchd; stop at explicit adoption when required; automatically replace stale app service; require exact handshake; create proof; remove verified Cargo binary; re-inspect; publish healthy. Also prove concurrent calls coalesce, blocked state does not self-retry, unrelated files become repairable drift, and final disagreement never reports healthy.

- [ ] **Step 2: Verify failure**

```bash
xcodebuild test -project PlugApp/PlugApp.xcodeproj -scheme PlugApp -destination 'platform=macOS' -only-testing:PlugAppTests/InstallationCoordinatorTests
```

- [ ] **Step 3: Implement minimal orchestration**

Keep one in-flight task. Set `reconcilingUpdate` phases only when visible work occurs. Convert actionable local conflicts to `repairableDrift`; operational failures to `blocked` with summary, underlying detail, and log URL. Never perform unbounded retry.

- [ ] **Step 4: Run and commit**

```bash
xcodebuild test -project PlugApp/PlugApp.xcodeproj -scheme PlugApp -destination 'platform=macOS'
git add PlugApp
git commit -m "feat: reconcile unified Plug installation"
```

### Task 6: Integrate startup, version handshake, and calm UI

**Files:**
- Modify: `PlugApp/PlugIPC/Sources/PlugIPC/PlugIPCClient.swift`
- Modify: `PlugApp/PlugApp/Stores/AppModel.swift`
- Modify: `PlugApp/PlugApp/App/PlugApplication.swift`
- Modify: `PlugApp/PlugApp/Views/RootView.swift`
- Modify: `PlugApp/PlugApp/Views/AppChrome.swift`
- Modify: `PlugApp/PlugApp/Menu/PlugMenu.swift`
- Modify: `PlugApp/PlugApp/Settings/SettingsView.swift`
- Modify: `PlugApp/PlugApp/Services/UpdateService.swift`
- Modify: `PlugApp/PlugAppTests/AppModelTests.swift`

**Interfaces:**

```swift
public init(socketURL: URL = defaultSocketURL, clientVersion: String)
```

- [ ] **Step 1: Write startup and presentation tests**

Prove Bundle-derived client version, coordinator-before-polling order, healthy silence, first-use `Use Plug`, delayed `Finishing Plug update…`, blocked `Retry`/`View Log`, snapshot preservation during repair, and no direct daemon restart action outside the coordinator.

- [ ] **Step 2: Verify failure**

```bash
swift test --package-path PlugApp/PlugIPC
xcodebuild test -project PlugApp/PlugApp.xcodeproj -scheme PlugApp -destination 'platform=macOS' -only-testing:PlugAppTests/AppModelTests
```

- [ ] **Step 3: Integrate coordinator and remove hardcoded version**

Supply `Bundle.main` short version to `PlugIPCClient`. Rename model startup to `start()`, reconcile before polling, and replace `serviceNeedsAdoption`, `adoptDaemon`, and `restartDaemon` with coordinator state/actions. Sparkle keeps normal replacement/relaunch; startup reconciliation is the commit point.

- [ ] **Step 4: Run Swift gates and commit**

```bash
swift test --package-path PlugApp/PlugIPC
xcodegen generate --spec PlugApp/project.yml
xcodebuild test -project PlugApp/PlugApp.xcodeproj -scheme PlugApp -destination 'platform=macOS'
git add PlugApp
git commit -m "feat: finish updates through one calm app workflow"
```

### Task 7: Prove signed-app reconciliation end to end

**Files:**
- Create or modify integration fixtures under: `PlugApp/PlugAppTests/Fixtures/`

- [ ] **Step 1: Run a signed local app fixture**

Create recognized Cargo, Formula, client-link, and launchd fixture state plus unrelated decoys. Launch the app, grant first adoption once, and assert exact-version daemon handshake, canonical shell link, repaired clients, Homebrew-driven Formula removal, verified Cargo removal, preserved decoys, and surviving stdio connector replay.

- [ ] **Step 2: Run all repository gates**

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
swift test --package-path PlugApp/PlugIPC
xcodebuild test -project PlugApp/PlugApp.xcodeproj -scheme PlugApp -destination 'platform=macOS'
```

- [ ] **Step 3: Commit fixture additions**

```bash
git add PlugApp
git commit -m "test: prove unified macOS reconciliation"
```
