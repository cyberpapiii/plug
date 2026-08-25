# Plug Native macOS App Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a calm native menu-bar and windowed Plug app with full human-facing parity over the daemon operator API.

**Architecture:** A macOS 14 SwiftUI app uses a small typed `PlugIPC` package and one `@Observable` application model. Views render immutable snapshots and submit daemon verbs; they never edit configuration, start child processes, or collect MCP credentials. The app bundles the release Rust binary and registers its LaunchAgent with `SMAppService`.

**Tech Stack:** Swift 6, SwiftUI, Observation, AppKit bridges, ServiceManagement, UserNotifications, XCTest, XCUITest, Xcode project with Swift Package Manager dependencies.

**Spec:** `docs/superpowers/specs/2026-08-25-plug-macos-app-design.md`

## Global Constraints

- Minimum macOS is 14.0.
- No App Sandbox; hardened runtime remains enabled.
- Plug.app never spawns `plug serve` with `Process`.
- Healthy UI is quiet; failures sort first and have one primary action.
- Only Plug-owned auth, adoption, and update events notify; notifications coalesce.
- Activity is metadata-only and observation-only.
- No charts, credential fields, MCP response controls, or cloud state.

---

### Task 1: Create the signed app shell and typed IPC package

**Files:**
- Create: `PlugApp/PlugApp.xcodeproj/project.pbxproj`
- Create: `PlugApp/PlugApp/Info.plist`
- Create: `PlugApp/PlugApp/PlugApp.entitlements`
- Create: `PlugApp/PlugApp/PlugApplication.swift`
- Create: `PlugApp/PlugIPC/Package.swift`
- Create: `PlugApp/PlugIPC/Sources/PlugIPC/FrameCodec.swift`
- Create: `PlugApp/PlugIPC/Sources/PlugIPC/PlugIPCClient.swift`
- Create: `PlugApp/PlugIPC/Sources/PlugIPC/ProtocolModels.swift`
- Create: `PlugApp/PlugIPC/Tests/PlugIPCTests/FrameCodecTests.swift`

**Interfaces:**
- Produces: `PlugIPCClient.connect(socketURL:)`, `PlugIPCClient.request(_:)`, `OperatorHandshake`, and a launchable `Plug.app` shell.

- [ ] **Step 1: Write frame-codec tests**

```swift
func testLengthPrefixedJSONRoundTrip() throws {
    let request = IPCRequest.operatorHandshake(clientVersion: "0.5.0", ipcMin: 3, ipcMax: 4)
    let frame = try FrameCodec.encode(request)
    XCTAssertEqual(frame.prefix(4), Data([0, 0, 0, UInt8(frame.count - 4)]))
    XCTAssertEqual(try FrameCodec.decode(IPCRequest.self, from: frame), request)
}
```

- [ ] **Step 2: Run the package test and verify sources are missing**

Run: `cd PlugApp/PlugIPC && swift test`

Expected: compile failure naming `FrameCodec`.

- [ ] **Step 3: Implement exact Rust-compatible framing and models**

Use a four-byte big-endian length and snake_case tagged JSON. Reject frames above 4 MiB before allocation. Model only operator variants used by the app; preserve unknown response fields through tolerant decoding.

- [ ] **Step 4: Build a single-connection actor**

```swift
public actor PlugIPCClient {
    public func connect(socketURL: URL) async throws -> OperatorHandshake
    public func request<Response: Decodable>(_ request: IPCRequest, as: Response.Type) async throws -> Response
    public func disconnect() async
}
```

Use `NWConnection` with a Unix endpoint if it supports the existing socket framing; otherwise use a narrow `SocketTransport` around Darwin `socket/connect/read/write`. Do not shell out to the CLI.

- [ ] **Step 5: Create the Xcode project and empty menu/window scenes**

`PlugApplication` declares `MenuBarExtra("Plug", systemImage: model.menuBarSymbol)` and one `WindowGroup`. Set `LSUIElement = true`, deployment target 14.0, and only the minimum non-sandbox entitlements.

Run: `xcodebuild -project PlugApp/PlugApp.xcodeproj -scheme PlugApp -destination 'platform=macOS' build && cd PlugApp/PlugIPC && swift test`

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add PlugApp
git commit -m "feat: scaffold native Plug app and IPC client"
```

### Task 2: Build the application state model and menu bar

**Files:**
- Create: `PlugApp/PlugApp/AppModel.swift`
- Create: `PlugApp/PlugApp/Models/OperatorSnapshot.swift`
- Create: `PlugApp/PlugApp/Menu/PlugMenu.swift`
- Create: `PlugApp/PlugAppTests/AppModelTests.swift`
- Modify: `PlugApp/PlugApp/PlugApplication.swift`

**Interfaces:**
- Produces: `AppModel.ConnectionState`, `AppModel.refresh()`, `AppModel.perform(_:)`, and `PlugMenu`.

- [ ] **Step 1: Write state-reduction and calm-sorting tests**

```swift
func testDegradedServersSortBeforeHealthyWithoutReorderingPeers() {
    let model = AppModel.preview(servers: [.healthy("A"), .failed("B"), .healthy("C")])
    XCTAssertEqual(model.visibleServers.map(\.name), ["B", "A", "C"])
    XCTAssertEqual(model.menuBarState, .degraded)
}
```

- [ ] **Step 2: Run the tests and verify `AppModel` is missing**

Run: `xcodebuild test -project PlugApp/PlugApp.xcodeproj -scheme PlugApp -destination 'platform=macOS' -only-testing:PlugAppTests/AppModelTests`

Expected: compile failure.

- [ ] **Step 3: Implement one observable state machine**

```swift
@MainActor @Observable final class AppModel {
    enum ConnectionState: Equatable { case disconnected, starting, incompatible(CompatibilityAction), ready }
    private(set) var connectionState: ConnectionState = .disconnected
    private(set) var snapshot: OperatorSnapshot = .empty
    private(set) var lastError: UserFacingError?
}
```

Connection errors do not erase the last useful snapshot. Refresh uses one in-flight task and coalesces repeated triggers.

- [ ] **Step 4: Build the menu-bar surface**

Show aggregate state, compact server rows, client count, pending auth count, and Open Plug / Start or Stop / Restart to Finish Update / View Logs. Hide irrelevant actions instead of disabling a long menu.

- [ ] **Step 5: Verify previews, VoiceOver labels, and tests**

Run: `xcodebuild test -project PlugApp/PlugApp.xcodeproj -scheme PlugApp -destination 'platform=macOS'`

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add PlugApp/PlugApp PlugApp/PlugAppTests
git commit -m "feat: add calm app state and menu bar"
```

### Task 3: Build Servers and Clients

**Files:**
- Create: `PlugApp/PlugApp/Root/RootView.swift`
- Create: `PlugApp/PlugApp/Servers/ServersView.swift`
- Create: `PlugApp/PlugApp/Servers/ServerDetailView.swift`
- Create: `PlugApp/PlugApp/Servers/AddServerView.swift`
- Create: `PlugApp/PlugApp/Clients/ClientsView.swift`
- Create: `PlugApp/PlugApp/Clients/ClientDetailView.swift`
- Create: `PlugApp/PlugApp/Clients/VisibilityMatrix.swift`
- Create: `PlugApp/PlugAppTests/ServerClientModelTests.swift`

**Interfaces:**
- Consumes: daemon validation/mutation/snapshot verbs.
- Produces: primary Servers screen and searchable client visibility matrix.

- [ ] **Step 1: Write mutation and visibility tests against a fake IPC client**

Prove enable, disable, restart, edit, remove, validate/add, unlink, and repair send one exact daemon request; prove the matrix derives visible tools from the authoritative snapshot rather than local guesses.

- [ ] **Step 2: Run tests and verify views/models are missing**

Run: `xcodebuild test -project PlugApp/PlugApp.xcodeproj -scheme PlugApp -destination 'platform=macOS' -only-testing:PlugAppTests/ServerClientModelTests`

Expected: compile failure.

- [ ] **Step 3: Implement Servers with inline actions**

Use a sidebar list/detail layout. Add Server accepts one pasted command or URL, sends `ValidateServer`, previews name/transport/auth, then sends `AddServer`. Errors remain attached to the affected row or form.

- [ ] **Step 4: Implement Clients and visibility matrix**

Default to a simple client list and detail. Reveal the matrix only when requested; rows are clients, columns/search results are tools, and cells use accessible checkmark/hidden labels rather than color alone.

- [ ] **Step 5: Run unit and UI smoke tests**

Run: `xcodebuild test -project PlugApp/PlugApp.xcodeproj -scheme PlugApp -destination 'platform=macOS'`

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add PlugApp/PlugApp/Root PlugApp/PlugApp/Servers PlugApp/PlugApp/Clients PlugApp/PlugAppTests
git commit -m "feat: add server and client management"
```

### Task 4: Build Activity, Auth, Settings, and diagnostics

**Files:**
- Create: `PlugApp/PlugApp/Activity/ActivityView.swift`
- Create: `PlugApp/PlugApp/Auth/AuthView.swift`
- Create: `PlugApp/PlugApp/Settings/SettingsView.swift`
- Create: `PlugApp/PlugApp/Diagnostics/DiagnosticsView.swift`
- Create: `PlugApp/PlugAppTests/SecondaryViewsTests.swift`

**Interfaces:**
- Produces: metadata-only activity filters, browser-routed auth actions, settings, doctor/status sheet.

- [ ] **Step 1: Write tests for filters, URL routing, and secret absence**

Serialize every Swift snapshot model and assert token/secret/params/result keys are absent. Verify reauth and consent open only daemon-returned HTTPS or loopback HTTP URLs.

- [ ] **Step 2: Run tests and verify components are missing**

Run: `xcodebuild test -project PlugApp/PlugApp.xcodeproj -scheme PlugApp -destination 'platform=macOS' -only-testing:PlugAppTests/SecondaryViewsTests`

Expected: compile failure.

- [ ] **Step 3: Implement Activity and Auth**

Activity uses a plain filterable table with time, client, method, server, latency, and outcome. Auth groups upstream reauth, downstream grants/revocation, and passkey state; every credential ceremony opens the existing browser flow.

- [ ] **Step 4: Implement Settings and diagnostics**

Settings contains launch at login, update channel, reveal config/logs, resolved config, validate config, and Uninstall Plug. Diagnostics shows a concise health result and copyable redacted detail.

- [ ] **Step 5: Run full Swift tests and commit**

```bash
xcodebuild test -project PlugApp/PlugApp.xcodeproj -scheme PlugApp -destination 'platform=macOS'
git add PlugApp
git commit -m "feat: add activity auth and settings"
```

### Task 5: Add service adoption, bounded recovery, and notifications

**Files:**
- Create: `PlugApp/PlugApp/Service/DaemonService.swift`
- Create: `PlugApp/PlugApp/Service/AdoptionView.swift`
- Create: `PlugApp/PlugApp/Notifications/NotificationCoordinator.swift`
- Create: `PlugApp/PlugApp/Resources/com.plug.daemon.plist`
- Create: `PlugApp/PlugAppTests/DaemonServiceTests.swift`
- Create: `PlugApp/PlugAppTests/NotificationCoordinatorTests.swift`

**Interfaces:**
- Produces: `DaemonService.register()`, `adopt()`, `restart()`, `unregister()`, and four coalesced notification classes.

- [ ] **Step 1: Write adoption, backoff, and coalescing tests**

Use injected service/clock/notification adapters. Prove retry delays are 100/200/400/800/1,600 ms, then stop; prove identical event keys replace delivered notifications; prove no MCP elicitation/sampling notification type exists.

- [ ] **Step 2: Run tests and verify service types are missing**

Run: `xcodebuild test -project PlugApp/PlugApp.xcodeproj -scheme PlugApp -destination 'platform=macOS' -only-testing:PlugAppTests/DaemonServiceTests -only-testing:PlugAppTests/NotificationCoordinatorTests`

Expected: compile failure.

- [ ] **Step 3: Implement `SMAppService` registration and adoption**

The LaunchAgent plist uses `BundleProgram` with a bundle-relative path to the embedded daemon. Adoption presents one confirmation, stops the old daemon gracefully, removes/supersedes stale CLI service state via daemon operator action, registers, kickstarts, and verifies the handshake.

- [ ] **Step 4: Implement explicit uninstall**

`Uninstall Plug…` confirms impact, calls `unregister()`, verifies the daemon stops, then opens or invokes the standard move-to-Trash flow. Closing the window or quitting never unregisters.

- [ ] **Step 5: Implement four coalesced notification routes**

Use stable identifiers for upstream reauth, downstream grant request, adoption, and update restart. Each notification opens the matching app/browser destination; none performs the decision inline.

- [ ] **Step 6: Run full app tests and commit**

```bash
xcodebuild test -project PlugApp/PlugApp.xcodeproj -scheme PlugApp -destination 'platform=macOS'
git add PlugApp
git commit -m "feat: add daemon adoption and notifications"
```

### Task 6: Prove the native app end to end

**Files:**
- Create: `PlugApp/PlugAppUITests/PlugAppUITests.swift`
- Create: `scripts/test-plug-app-e2e.sh`
- Modify: `docs/PROJECT-STATE-SNAPSHOT.md`
- Modify: `docs/PLAN.md`

**Interfaces:**
- Consumes: all prior app and daemon tasks.
- Produces: a tested, unsigned local app ready for the distribution plan.

- [ ] **Step 1: Add critical UI flows**

Test first launch/adoption, healthy overview, one degraded server, add/disable/restart/remove, client visibility, reauth routing, bounded start failure, and update-restart prompt against an isolated daemon fixture.

- [ ] **Step 2: Prove CLI/GUI parity**

`scripts/test-plug-app-e2e.sh` builds both products, starts an isolated daemon, mutates a server through CLI and observes it in app IPC, mutates through app test harness and observes it in CLI JSON, then shuts down cleanly.

- [ ] **Step 3: Run all local gates**

```bash
xcodebuild test -project PlugApp/PlugApp.xcodeproj -scheme PlugApp -destination 'platform=macOS'
scripts/test-plug-app-e2e.sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

Expected: every command exits 0.

- [ ] **Step 4: Update current-truth docs and commit**

```bash
git add PlugApp scripts/test-plug-app-e2e.sh docs/PROJECT-STATE-SNAPSHOT.md docs/PLAN.md
git commit -m "test: certify native Plug app parity"
```
