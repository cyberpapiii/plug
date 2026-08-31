import Darwin
import Foundation
import XCTest
@testable import Plug
import PlugIPC

private let currentTestAppVersion = PlugIPCClient.clientVersion(
    from: Bundle.main.infoDictionary ?? [:]
)

/// The operator token normally lives in the user's Application Support
/// directory. Tests that must reach `.ready` write their own so the suite
/// does not depend on a real Plug installation on the host.
private func makeFixtureTokenURL() throws -> URL {
    let url = URL(fileURLWithPath: "/tmp/plug-app-model-token-\(UUID().uuidString)")
    try "fixture-token".write(to: url, atomically: true, encoding: .utf8)
    return url
}

final class AppModelTests: XCTestCase {
    /// A model that has not asked yet reports `.connecting`, not
    /// `.disconnected`. The difference is what the menu bar says while Plug
    /// starts: "Starting…" rather than "Plug is not running".
    @MainActor func testEmptyModelHasNotAskedYet() {
        let model = AppModel()
        XCTAssertEqual(model.connectionState, .connecting)
        XCTAssertTrue(model.visibleServers.isEmpty)
    }

    func testBundleClientVersionFallback() {
        XCTAssertEqual(PlugIPCClient.clientVersion(from: [:]), "development")
        XCTAssertEqual(
            PlugIPCClient.clientVersion(from: ["CFBundleShortVersionString": "  "]),
            "development"
        )
        XCTAssertEqual(
            PlugIPCClient.clientVersion(from: ["CFBundleShortVersionString": "0.7.0"]),
            "0.7.0"
        )
    }

    @MainActor
    func testInstallationStateAlwaysGatesOverallHealth() async {
        let coordinator = RecordingInstallationCoordinator(
            state: .healthy(makeInstallationSnapshot()),
            events: LockedEvents()
        )
        let server = try! OperatorFixtureServer(events: coordinator.events)
        defer { server.stop() }

        let model = AppModel(
            ipc: PlugIPCClient(socketURL: server.socketURL, clientVersion: currentTestAppVersion),
            coordinator: coordinator,
            tokenURL: try! makeFixtureTokenURL()
        )
        await model.start()
        XCTAssertTrue(model.isHealthy)

        let nonHealthyStates: [InstallationState] = [
            .adoptionRequired(makeInstallationSnapshot()),
            .reconcilingUpdate(.verifying),
            .repairableDrift(InstallationDrift(summary: "drift", detail: "detail")),
            .blocked(InstallationFailure(summary: "blocked", detail: "detail", logURL: nil)),
        ]
        for state in nonHealthyStates {
            coordinator.state = state
            await model.reconcile(trigger: .retry)
            XCTAssertFalse(model.isHealthy, "\(state) must not report healthy")
        }
    }

    @MainActor func testLegacyConnectorDiscoveryOnlyTargetsPlugConnectProcesses() {
        let output = """
          101 /Users/me/.local/bin/plug connect
          102 /Users/me/.cargo/bin/plug --config /tmp/config.toml connect
          103 /Users/me/.local/bin/plug serve --daemon
          104 /usr/bin/python watchdog.py -- /Users/me/.local/bin/plug connect
          105 /bin/zsh -c rg 'plug connect'
        """
        XCTAssertEqual(DaemonServiceManager.connectorPIDs(psOutput: output), [101, 102])
    }

    @MainActor
    func testStartupRunsInstallationReconciliationBeforePolling() async {
        let events = LockedEvents()
        let coordinator = RecordingInstallationCoordinator(
            state: .healthy(makeInstallationSnapshot()),
            events: events
        )
        let server = try! OperatorFixtureServer(events: events)
        defer { server.stop() }

        let model = AppModel(
            ipc: PlugIPCClient(socketURL: server.socketURL, clientVersion: currentTestAppVersion),
            coordinator: coordinator
        )
        await model.start()

        XCTAssertEqual(events.values.prefix(2), ["coordinator.reconcile", "ipc.handshake"])
    }

    /// The app opens in `.reconcilingUpdate(.inspecting)` on every launch,
    /// before it has looked at anything. That phase only reads, so a working
    /// install must not be told it is being set up; the phases that do change
    /// the install still say so.
    @MainActor
    func testLaunchInspectionSaysStartingNotInstalling() {
        let inspecting = AppModel(
            coordinator: RecordingInstallationCoordinator(
                state: .reconcilingUpdate(.inspecting),
                events: LockedEvents()
            )
        )
        XCTAssertEqual(inspecting.verdict.title, "Starting…")

        let installing = AppModel(
            coordinator: RecordingInstallationCoordinator(
                state: .reconcilingUpdate(.replacingDaemon),
                events: LockedEvents()
            )
        )
        XCTAssertEqual(installing.verdict.title, "Setting up…")
    }

    @MainActor
    func testHealthyStartupStaysQuiet() async {
        let coordinator = RecordingInstallationCoordinator(
            state: .healthy(makeInstallationSnapshot()),
            events: LockedEvents()
        )
        let server = try! OperatorFixtureServer(events: coordinator.events)
        defer { server.stop() }

        let model = AppModel(
            ipc: PlugIPCClient(socketURL: server.socketURL, clientVersion: currentTestAppVersion),
            coordinator: coordinator
        )
        await model.start()

        XCTAssertNil(model.installationFailure)
    }

    /// The handshake describes the daemon behind an open descriptor, so a poll
    /// that already has one is asking a question it holds the answer to. At two
    /// seconds a poll, that was a wasted round trip every two seconds.
    @MainActor
    func testPollingDoesNotRenegotiateAnOpenConnection() async {
        let coordinator = RecordingInstallationCoordinator(
            state: .healthy(makeInstallationSnapshot()),
            events: LockedEvents()
        )
        let server = try! OperatorFixtureServer(events: coordinator.events)
        defer { server.stop() }

        let model = AppModel(
            ipc: PlugIPCClient(socketURL: server.socketURL, clientVersion: currentTestAppVersion),
            coordinator: coordinator,
            tokenURL: try! makeFixtureTokenURL()
        )
        await model.start()
        await model.refresh()
        await model.refresh()

        let handshakes = coordinator.events.values.filter { $0 == "ipc.handshake" }
        XCTAssertEqual(handshakes.count, 1)
        let snapshots = coordinator.events.values.filter { $0 == "ipc.snapshot" }
        XCTAssertGreaterThan(snapshots.count, 1, "the polls themselves must still have happened")
    }

    /// The tool list is by far the largest thing the daemon can be asked for.
    /// It used to be refetched on a timer; the snapshot now reports when it
    /// would answer differently, so a poll that sees the same revision must not
    /// ask for it again.
    @MainActor
    func testUnchangedCatalogRevisionSkipsTheToolList() async {
        let coordinator = RecordingInstallationCoordinator(
            state: .healthy(makeInstallationSnapshot()),
            events: LockedEvents()
        )
        let server = try! OperatorFixtureServer(events: coordinator.events)
        defer { server.stop() }

        let model = AppModel(
            ipc: PlugIPCClient(socketURL: server.socketURL, clientVersion: currentTestAppVersion),
            coordinator: coordinator,
            tokenURL: try! makeFixtureTokenURL()
        )
        await model.start()
        await model.refresh()
        await model.refresh()
        XCTAssertEqual(coordinator.events.values.filter { $0 == "ipc.listTools" }.count, 1)

        server.catalogRevision = 2
        await model.refresh()
        XCTAssertEqual(coordinator.events.values.filter { $0 == "ipc.listTools" }.count, 2)
    }

    @MainActor
    func testUsePlugIsExplicitCoordinatorAction() async {
        let coordinator = RecordingInstallationCoordinator(
            state: .adoptionRequired(makeInstallationSnapshot()),
            events: LockedEvents()
        )
        let model = AppModel(
            ipc: PlugIPCClient(socketURL: URL(fileURLWithPath: "/tmp/plug-no-socket"), clientVersion: currentTestAppVersion),
            coordinator: coordinator
        )

        await model.adopt()

        XCTAssertEqual(coordinator.events.values, ["coordinator.adopt", "coordinator.reconcile"])
    }

    /// Work in progress has to describe itself while it is in progress. The app
    /// used to keep its own copy of the installation state and refresh it only
    /// when reconciliation ended, so a repair spent its whole run describing the
    /// situation from before it started.
    @MainActor
    func testAReconciliationInProgressDescribesItselfAndPreservesSnapshot() async {
        let events = LockedEvents()
        let gate = AsyncGate()
        let coordinator = RecordingInstallationCoordinator(
            state: .healthy(makeInstallationSnapshot()),
            events: events
        )
        let server = try! OperatorFixtureServer(events: events)
        defer { server.stop() }

        let model = AppModel(
            ipc: PlugIPCClient(socketURL: server.socketURL, clientVersion: currentTestAppVersion),
            coordinator: coordinator
        )
        await model.start()
        let originalSnapshot = model.snapshot

        coordinator.operation = {
            coordinator.state = .reconcilingUpdate(.replacingDaemon)
            await gate.wait()
        }
        let retry = Task { await model.retry() }
        await gate.enteredWait()
        XCTAssertEqual(model.verdict.title, "Setting up…")
        XCTAssertEqual(model.snapshot, originalSnapshot)
        coordinator.state = .healthy(makeInstallationSnapshot())
        await gate.release()
        await retry.value
        XCTAssertNotEqual(model.verdict.title, "Setting up…")
    }

    @MainActor
    func testBlockedStateOffersRetryAndViewLogThroughCoordinator() async {
        let logURL = URL(fileURLWithPath: "/tmp/plug-reconciliation.log")
        let coordinator = RecordingInstallationCoordinator(
            state: .blocked(InstallationFailure(
                summary: "Plug needs attention",
                detail: "daemon skew",
                logURL: logURL
            )),
            events: LockedEvents()
        )
        let model = AppModel(
            ipc: PlugIPCClient(socketURL: URL(fileURLWithPath: "/tmp/plug-no-socket"), clientVersion: currentTestAppVersion),
            coordinator: coordinator
        )

        XCTAssertEqual(model.installationFailure?.summary, "Plug needs attention")
        await model.retry()
        model.openLog()

        XCTAssertEqual(coordinator.events.values, ["coordinator.retry", "coordinator.reconcile", "coordinator.openLog"])
    }

    func testIPCClientAcceptsCallerSuppliedClientVersion() async throws {
        let events = LockedEvents()
        let server = try OperatorFixtureServer(events: events)
        defer { server.stop() }
        let client = PlugIPCClient(socketURL: server.socketURL, clientVersion: "9.9.9")

        _ = try await client.connect()

        XCTAssertEqual(server.clientVersion, "9.9.9")
    }

    @MainActor
    func testDaemonVersionSkewRetriesThroughCoordinator() async {
        let events = LockedEvents()
        let coordinator = RecordingInstallationCoordinator(
            state: .healthy(makeInstallationSnapshot()),
            events: events
        )
        let server = try! OperatorFixtureServer(events: events, daemonVersion: "0.6.3")
        defer { server.stop() }

        let model = AppModel(
            ipc: PlugIPCClient(socketURL: server.socketURL, clientVersion: currentTestAppVersion),
            coordinator: coordinator
        )
        await model.start()

        XCTAssertTrue(events.values.contains("coordinator.retry"))
        XCTAssertEqual(model.connectionState, .incompatible)
        XCTAssertEqual(
            model.connectionRecoveryDetail,
            "The app and its background service are running different versions."
        )
    }

    @MainActor
    func testIncompatibleIPCExposesOneRetryReconcileAction() async {
        let events = LockedEvents()
        let coordinator = RecordingInstallationCoordinator(
            state: .healthy(makeInstallationSnapshot()),
            events: events
        )
        let server = try! OperatorFixtureServer(events: events, ipcMin: 7, ipcMax: 7)
        defer { server.stop() }

        let model = AppModel(
            ipc: PlugIPCClient(socketURL: server.socketURL, clientVersion: currentTestAppVersion),
            coordinator: coordinator
        )
        await model.start()

        XCTAssertEqual(model.connectionState, .incompatible)
        XCTAssertTrue(model.connectionRecoveryIsRequired)
        XCTAssertFalse(model.isHealthy)
        XCTAssertEqual(
            model.connectionRecoveryDetail,
            "The app and its background service are running different versions."
        )

        await model.retryConnection()

        XCTAssertTrue(events.values.contains("coordinator.retry"))
        XCTAssertEqual(model.connectionState, .incompatible)
        XCTAssertTrue(model.connectionRecoveryIsRequired)
    }

    @MainActor
    func testRetryDisconnectsStaleIPCBeforeDaemonSocketReplacement() async throws {
        let events = LockedEvents()
        let socketURL = URL(
            fileURLWithPath: "/tmp/plug-app-model-replacement-\(UUID().uuidString).sock"
        )
        let oldServer = try OperatorFixtureServer(
            events: events,
            ipcMin: 7,
            ipcMax: 7,
            socketURL: socketURL
        )
        var replacement: OperatorFixtureServer?
        defer {
            if let replacement {
                replacement.stop()
            } else {
                oldServer.stop()
            }
        }

        let coordinator = RecordingInstallationCoordinator(
            state: .healthy(makeInstallationSnapshot()),
            events: events
        )
        let model = AppModel(
            ipc: PlugIPCClient(socketURL: socketURL, clientVersion: currentTestAppVersion),
            coordinator: coordinator,
            tokenURL: try makeFixtureTokenURL()
        )
        await model.start()
        XCTAssertEqual(model.connectionState, .incompatible)

        coordinator.operation = {
            events.append("coordinator.replaceSocket")
            oldServer.stop()
            replacement = try? OperatorFixtureServer(
                events: events,
                socketURL: socketURL
            )
        }

        await model.retryConnection()

        XCTAssertEqual(model.connectionState, .ready)
        let values = events.values
        guard let replacementIndex = values.firstIndex(of: "coordinator.replaceSocket") else {
            return XCTFail("Expected coordinator to replace daemon socket")
        }
        guard let reconnectIndex = values.indices.first(where: {
            $0 > replacementIndex && values[$0] == "ipc.handshake"
        }) else {
            return XCTFail("Expected IPC handshake after daemon socket replacement")
        }
        XCTAssertGreaterThan(reconnectIndex, replacementIndex)
    }

    @MainActor
    func testNotificationsStaySilentInitiallyAndDeduplicateTransitions() {
        var postedIDs: [String] = []
        let service = NotificationService { id, _, _ in postedIDs.append(id) }
        let empty = makeNotificationSnapshot()
        let authenticated = makeNotificationSnapshot(authenticated: true)
        let unauthenticated = makeNotificationSnapshot(authenticated: false)
        let newClient = makeNotificationSnapshot(authenticated: false, includeClient: true)

        service.observe(empty)
        service.observe(empty)
        XCTAssertTrue(postedIDs.isEmpty)

        service.observe(authenticated)
        service.observe(authenticated)
        XCTAssertTrue(postedIDs.isEmpty)

        service.observe(unauthenticated)
        service.observe(unauthenticated)
        XCTAssertEqual(postedIDs, ["upstream-reauth-alpha"])

        service.observe(newClient)
        service.observe(newClient)
        XCTAssertEqual(
            postedIDs,
            ["upstream-reauth-alpha", "downstream-client-client-1"]
        )
    }

    @MainActor
    func testCanReadServerConfigFollowsHandshakeCapability() async throws {
        let withCapability = try OperatorFixtureServer(
            events: LockedEvents(),
            ipcMax: 6,
            capabilities: ["server_config_read"]
        )
        defer { withCapability.stop() }
        let readable = AppModel(
            ipc: PlugIPCClient(socketURL: withCapability.socketURL, clientVersion: currentTestAppVersion),
            coordinator: RecordingInstallationCoordinator(
                state: .healthy(makeInstallationSnapshot()),
                events: LockedEvents()
            ),
            tokenURL: try makeFixtureTokenURL()
        )
        await readable.start()
        XCTAssertTrue(readable.canReadServerConfig)

        let withoutCapability = try OperatorFixtureServer(events: LockedEvents())
        defer { withoutCapability.stop() }
        let blocked = AppModel(
            ipc: PlugIPCClient(socketURL: withoutCapability.socketURL, clientVersion: currentTestAppVersion),
            coordinator: RecordingInstallationCoordinator(
                state: .healthy(makeInstallationSnapshot()),
                events: LockedEvents()
            ),
            tokenURL: try makeFixtureTokenURL()
        )
        await blocked.start()
        XCTAssertFalse(blocked.canReadServerConfig)
    }

    @MainActor
    func testServerConfigLoadsWhenCapabilityIsPresent() async throws {
        let events = LockedEvents()
        let server = try OperatorFixtureServer(
            events: events,
            ipcMax: 6,
            capabilities: ["server_config_read"]
        )
        defer { server.stop() }
        let model = AppModel(
            ipc: PlugIPCClient(socketURL: server.socketURL, clientVersion: currentTestAppVersion),
            coordinator: RecordingInstallationCoordinator(
                state: .healthy(makeInstallationSnapshot()),
                events: events
            ),
            tokenURL: try makeFixtureTokenURL()
        )
        await model.start()

        let config = try await model.serverConfig(name: "workspace")
        XCTAssertEqual(config.command, "npx")
        XCTAssertTrue(events.values.contains("ipc.getServerConfig"))
    }

    @MainActor
    func testServerConfigDoesNotFireWithoutCapability() async throws {
        let events = LockedEvents()
        let server = try OperatorFixtureServer(events: events)
        defer { server.stop() }
        let model = AppModel(
            ipc: PlugIPCClient(socketURL: server.socketURL, clientVersion: currentTestAppVersion),
            coordinator: RecordingInstallationCoordinator(
                state: .healthy(makeInstallationSnapshot()),
                events: events
            ),
            tokenURL: try makeFixtureTokenURL()
        )
        await model.start()
        XCTAssertFalse(model.canReadServerConfig)

        do {
            _ = try await model.serverConfig(name: "workspace")
            XCTFail("GetServerConfig must not succeed without server_config_read")
        } catch {
            XCTAssertEqual(
                error.localizedDescription,
                AppModel.serverConfigReadRequiredCopy
            )
            XCTAssertFalse(
                error.localizedDescription.contains("PARSE_ERROR"),
                "missing capability must not surface as a parse failure"
            )
        }
        XCTAssertFalse(events.values.contains("ipc.getServerConfig"))
    }

    @MainActor
    func testOlderHandshakeWithoutV6BlocksGetServerConfig() async throws {
        let events = LockedEvents()
        let server = try OperatorFixtureServer(
            events: events,
            ipcMin: 3,
            ipcMax: 4
        )
        defer { server.stop() }
        let model = AppModel(
            ipc: PlugIPCClient(socketURL: server.socketURL, clientVersion: currentTestAppVersion),
            coordinator: RecordingInstallationCoordinator(
                state: .healthy(makeInstallationSnapshot()),
                events: events
            ),
            tokenURL: try makeFixtureTokenURL()
        )
        await model.start()
        XCTAssertEqual(model.connectionState, .ready)
        XCTAssertFalse(model.canReadServerConfig)

        do {
            _ = try await model.serverConfig(name: "workspace")
            XCTFail("older handshake without v6 must not load server config")
        } catch {
            XCTAssertEqual(error.localizedDescription, AppModel.serverConfigReadRequiredCopy)
        }
        XCTAssertFalse(events.values.contains("ipc.getServerConfig"))
    }

    @MainActor
    private func makeInstallationSnapshot() -> InstallationSnapshot {
        let executable = URL(fileURLWithPath: "/Applications/Plug.app/Contents/Resources/plug")
        let app = VerifiedAppInstallation(
            bundleURL: executable.deletingLastPathComponent().deletingLastPathComponent(),
            executableURL: executable,
            appVersion: currentTestAppVersion,
            buildVersion: "12",
            embeddedVersion: currentTestAppVersion,
            teamID: AppInstallationInspector.teamID
        )
        let record = LaunchdJobRecord(
            label: "com.plug.daemon",
            programURL: executable,
            parentBundleIdentifier: AppInstallationInspector.bundleIdentifier,
            parentBundleVersion: "12",
            loaded: true
        )
        let service = DaemonServiceSnapshot(
            ownership: .appManagedCurrent(record),
            daemonVersion: currentTestAppVersion,
            daemonExecutable: executable
        )
        return InstallationSnapshot(
            app: app,
            shellLink: .canonical(executable),
            service: service,
            daemonVersion: currentTestAppVersion,
            clientRepairNeeded: false,
            shadowInstalls: []
        )
    }

    @MainActor
    private func makeNotificationSnapshot(
        authenticated: Bool = true,
        includeClient: Bool = false
    ) -> OperatorSnapshot {
        let clientJSON = includeClient
            ? #"[{"clientId":"client-1","clientName":"Client","redirectUris":[],"source":"test"}]"#
            : "[]"
        let payload = """
        {
          "runtimeVersion": "\(currentTestAppVersion)",
          "uptimeSecs": 1,
          "ownership": "app_managed",
          "configuredServers": [],
          "servers": [],
          "liveSessions": [],
          "clientVisibility": [],
          "upstreamAuth": [{
            "name": "alpha",
            "url": null,
            "authenticated": \(authenticated),
            "health": "Healthy",
            "scopes": null,
            "tokenExpiresInSecs": null,
            "warnings": []
          }],
          "downstreamClients": \(clientJSON)
        }
        """
        return try! JSONDecoder().decode(OperatorSnapshot.self, from: Data(payload.utf8))
    }
}

@MainActor
private final class RecordingInstallationCoordinator: InstallationCoordinating {
    var state: InstallationState
    let events: LockedEvents
    var operation: (() async -> Void)?

    init(
        state: InstallationState,
        events: LockedEvents,
        operation: (() async -> Void)? = nil
    ) {
        self.state = state
        self.events = events
        self.operation = operation
    }

    func reconcile(trigger: ReconciliationTrigger) async {
        events.append("coordinator.reconcile")
        await operation?()
    }

    func adopt() async {
        events.append("coordinator.adopt")
        await reconcile(trigger: .explicitAdoption)
    }

    func retry() async {
        events.append("coordinator.retry")
        await reconcile(trigger: .retry)
    }

    func openLog() {
        events.append("coordinator.openLog")
    }
}

private final class LockedEvents: @unchecked Sendable {
    private let lock = NSLock()
    private var storage: [String] = []

    var values: [String] {
        lock.lock(); defer { lock.unlock() }
        return storage
    }

    func append(_ value: String) {
        lock.lock(); defer { lock.unlock() }
        storage.append(value)
    }
}

private actor AsyncGate {
    private var entered = false
    private var released = false
    private var enteredWaiters: [CheckedContinuation<Void, Never>] = []
    private var releaseWaiters: [CheckedContinuation<Void, Never>] = []

    func wait() async {
        entered = true
        enteredWaiters.forEach { $0.resume() }
        enteredWaiters.removeAll()
        if !released {
            await withCheckedContinuation { releaseWaiters.append($0) }
        }
    }

    func enteredWait() async {
        if entered { return }
        await withCheckedContinuation { enteredWaiters.append($0) }
    }

    func release() {
        released = true
        releaseWaiters.forEach { $0.resume() }
        releaseWaiters.removeAll()
    }
}

private final class OperatorFixtureServer: @unchecked Sendable {
    let socketURL: URL
    private let listener: Int32
    private let events: LockedEvents
    private let daemonVersion: String
    private let ipcMin: UInt16
    private let ipcMax: UInt16
    private let capabilities: [String]
    private let lock = NSLock()
    private let serveStopped = DispatchSemaphore(value: 0)
    private(set) var clientVersion: String?
    /// Bumped by a test to say the daemon's tool catalog would now answer
    /// differently. Read from the serve thread, written from the test thread.
    var catalogRevision: UInt64 {
        get { lock.lock(); defer { lock.unlock() }; return storedCatalogRevision }
        set { lock.lock(); storedCatalogRevision = newValue; lock.unlock() }
    }
    private var storedCatalogRevision: UInt64 = 1
    private var connection: Int32 = -1
    private var didStop = false

    init(
        events: LockedEvents,
        daemonVersion: String = currentTestAppVersion,
        ipcMin: UInt16 = 3,
        ipcMax: UInt16 = 4,
        capabilities: [String] = [],
        socketURL providedSocketURL: URL? = nil
    ) throws {
        self.events = events
        self.daemonVersion = daemonVersion
        self.ipcMin = ipcMin
        self.ipcMax = ipcMax
        self.capabilities = capabilities
        self.socketURL = providedSocketURL
            ?? URL(fileURLWithPath: "/tmp/plug-app-model-\(UUID().uuidString).sock")
        listener = Darwin.socket(AF_UNIX, SOCK_STREAM, 0)
        guard listener >= 0 else { throw PlugIPCError.systemCall("socket", errno) }
        Darwin.unlink(self.socketURL.path)
        var address = sockaddr_un()
        address.sun_family = sa_family_t(AF_UNIX)
        let bytes = Array(self.socketURL.path.utf8CString)
        withUnsafeMutableBytes(of: &address.sun_path) { destination in
            bytes.withUnsafeBytes { source in destination.copyBytes(from: source) }
        }
        let pathOffset = MemoryLayout<sockaddr_un>.offset(of: \sockaddr_un.sun_path)!
        let addressLength = socklen_t(pathOffset + bytes.count)
        address.sun_len = UInt8(addressLength)
        let bound = withUnsafePointer(to: &address) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                Darwin.bind(listener, $0, addressLength)
            }
        }
        guard bound == 0, Darwin.listen(listener, 1) == 0 else {
            let code = errno
            Darwin.close(listener)
            throw PlugIPCError.systemCall("listen", code)
        }
        DispatchQueue.global(qos: .userInitiated).async { [weak self] in self?.serve() }
    }

    func stop() {
        lock.lock()
        guard !didStop else {
            lock.unlock()
            return
        }
        didStop = true
        let connection = self.connection
        self.connection = -1
        lock.unlock()

        if connection >= 0 {
            Darwin.shutdown(connection, SHUT_RDWR)
            Darwin.close(connection)
        }
        Darwin.shutdown(listener, SHUT_RDWR)
        Darwin.close(listener)
        Darwin.unlink(socketURL.path)
        XCTAssertEqual(
            serveStopped.wait(timeout: .now() + 2),
            .success,
            "Fixture server did not finish before its socket path was reused"
        )
    }

    private func serve() {
        defer { serveStopped.signal() }
        while true {
            let accepted = Darwin.accept(listener, nil, nil)
            guard accepted >= 0 else { return }
            lock.lock(); connection = accepted; lock.unlock()
            while true {
                guard let header = readExact(accepted, count: 4) else { break }
                let length = header.reduce(UInt32.zero) { ($0 << 8) | UInt32($1) }
                guard let payload = readExact(accepted, count: Int(length)),
                      let object = try? JSONSerialization.jsonObject(with: payload) as? [String: Any],
                      let type = object["type"] as? String
                else { break }
                switch type {
                case "OperatorHandshake":
                    clientVersion = object["client_version"] as? String
                    events.append("ipc.handshake")
                    send(response: [
                        "type": "OperatorHandshake",
                        "handshake": [
                            "daemon_version": daemonVersion, "ipc_min": ipcMin, "ipc_max": ipcMax,
                            "ownership": "app", "capabilities": capabilities,
                        ],
                    ], to: accepted)
                case "GetServerConfig":
                    events.append("ipc.getServerConfig")
                    send(response: [
                        "type": "ServerConfig",
                        "name": object["name"] as? String ?? "",
                        "server": [
                            "command": "npx",
                            "args": ["-y", "linear-mcp"],
                            "transport": "stdio",
                        ],
                    ], to: accepted)
                case "OperatorSnapshot":
                    events.append("ipc.snapshot")
                    send(response: [
                        "type": "OperatorSnapshot",
                        "snapshot": [
                            "runtime_version": currentTestAppVersion, "uptime_secs": 1, "ownership": "app",
                            "tool_catalog_revision": catalogRevision,
                            "configured_servers": [], "servers": [], "live_sessions": [],
                            "client_visibility": [], "upstream_auth": [], "downstream_clients": [],
                        ],
                    ], to: accepted)
                case "ListTools":
                    events.append("ipc.listTools")
                    send(response: [
                        "type": "Tools",
                        "tools": [[
                            "name": "demo__echo", "server_id": "demo", "description": "Echo.",
                            "title": "Echo", "disabled": false,
                        ]],
                    ], to: accepted)
                case "ActivitySnapshot":
                    events.append("ipc.activity")
                    send(response: ["type": "ActivitySnapshot", "events": []], to: accepted)
                default:
                    send(response: ["type": "Ok"], to: accepted)
                }
            }
            lock.lock()
            let ownsConnection = connection == accepted
            if ownsConnection { connection = -1 }
            lock.unlock()
            if ownsConnection { Darwin.close(accepted) }
        }
    }

    private func send(response: [String: Any], to fd: Int32) {
        guard let payload = try? JSONSerialization.data(withJSONObject: response) else { return }
        var frame = Data([
            UInt8((payload.count >> 24) & 0xff), UInt8((payload.count >> 16) & 0xff),
            UInt8((payload.count >> 8) & 0xff), UInt8(payload.count & 0xff),
        ])
        frame.append(payload)
        frame.withUnsafeBytes { raw in _ = Darwin.write(fd, raw.baseAddress!, raw.count) }
    }

    private func readExact(_ fd: Int32, count: Int) -> Data? {
        var data = Data(count: count)
        let result = data.withUnsafeMutableBytes { raw -> Int in
            var offset = 0
            while offset < count {
                let readCount = Darwin.read(fd, raw.baseAddress!.advanced(by: offset), count - offset)
                guard readCount > 0 else { return -1 }
                offset += readCount
            }
            return offset
        }
        return result == count ? data : nil
    }
}
