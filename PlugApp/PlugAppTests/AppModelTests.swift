import Darwin
import Foundation
import XCTest
@testable import Plug
import PlugIPC

final class AppModelTests: XCTestCase {
    @MainActor func testEmptyModelIsQuietlyDisconnected() {
        let model = AppModel()
        XCTAssertEqual(model.connectionState, .disconnected)
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
            ipc: PlugIPCClient(socketURL: server.socketURL, clientVersion: "0.6.4"),
            coordinator: coordinator
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
            ipc: PlugIPCClient(socketURL: server.socketURL, clientVersion: "0.6.4"),
            coordinator: coordinator
        )
        await model.start()

        XCTAssertEqual(events.values.prefix(2), ["coordinator.reconcile", "ipc.handshake"])
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
            ipc: PlugIPCClient(socketURL: server.socketURL, clientVersion: "0.6.4"),
            coordinator: coordinator
        )
        await model.start()

        XCTAssertFalse(model.showsReconciliationProgress)
        XCTAssertNil(model.installationFailure)
    }

    @MainActor
    func testUsePlugIsExplicitCoordinatorAction() async {
        let coordinator = RecordingInstallationCoordinator(
            state: .adoptionRequired(makeInstallationSnapshot()),
            events: LockedEvents()
        )
        let model = AppModel(
            ipc: PlugIPCClient(socketURL: URL(fileURLWithPath: "/tmp/plug-no-socket"), clientVersion: "0.6.4"),
            coordinator: coordinator
        )

        await model.adopt()

        XCTAssertEqual(coordinator.events.values, ["coordinator.adopt", "coordinator.reconcile"])
    }

    @MainActor
    func testLongReconciliationShowsDelayedFinishingMessageAndPreservesSnapshot() async {
        let events = LockedEvents()
        let gate = AsyncGate()
        let coordinator = RecordingInstallationCoordinator(
            state: .healthy(makeInstallationSnapshot()),
            events: events
        )
        let server = try! OperatorFixtureServer(events: events)
        defer { server.stop() }

        let model = AppModel(
            ipc: PlugIPCClient(socketURL: server.socketURL, clientVersion: "0.6.4"),
            coordinator: coordinator
        )
        await model.start()
        let originalSnapshot = model.snapshot

        coordinator.operation = { await gate.wait() }
        let retry = Task { await model.retry() }
        await gate.enteredWait()
        try? await Task.sleep(for: .milliseconds(80))
        XCTAssertFalse(model.showsReconciliationProgress)
        try? await Task.sleep(for: .milliseconds(400))
        XCTAssertTrue(model.showsReconciliationProgress)
        XCTAssertEqual(model.snapshot, originalSnapshot)
        await gate.release()
        await retry.value
        XCTAssertFalse(model.showsReconciliationProgress)
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
            ipc: PlugIPCClient(socketURL: URL(fileURLWithPath: "/tmp/plug-no-socket"), clientVersion: "0.6.4"),
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
            ipc: PlugIPCClient(socketURL: server.socketURL, clientVersion: "0.6.4"),
            coordinator: coordinator
        )
        await model.start()

        XCTAssertTrue(events.values.contains("coordinator.retry"))
        XCTAssertEqual(model.connectionState, .incompatible)
    }

    @MainActor
    func testIncompatibleIPCExposesOneRetryReconcileAction() async {
        let events = LockedEvents()
        let coordinator = RecordingInstallationCoordinator(
            state: .healthy(makeInstallationSnapshot()),
            events: events
        )
        let server = try! OperatorFixtureServer(events: events, ipcMin: 5, ipcMax: 6)
        defer { server.stop() }

        let model = AppModel(
            ipc: PlugIPCClient(socketURL: server.socketURL, clientVersion: "0.6.4"),
            coordinator: coordinator
        )
        await model.start()

        XCTAssertEqual(model.connectionState, .incompatible)
        XCTAssertTrue(model.connectionRecoveryIsRequired)
        XCTAssertFalse(model.isHealthy)

        await model.retryConnection()

        XCTAssertTrue(events.values.contains("coordinator.retry"))
        XCTAssertEqual(model.connectionState, .incompatible)
        XCTAssertTrue(model.connectionRecoveryIsRequired)
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

    func testAppModelHasNoDirectDaemonRestartOrProcessSpawnBypass() throws {
        let sourceURL = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent()
            .appending(path: "../PlugApp/Stores/AppModel.swift")
            .standardizedFileURL
        let source = try String(contentsOf: sourceURL, encoding: .utf8)

        XCTAssertFalse(source.contains("DaemonServiceManager.shared"))
        XCTAssertFalse(source.contains("restartDaemon"))
        XCTAssertFalse(source.contains("launchctl"))
        XCTAssertFalse(source.contains("Process("))
    }

    @MainActor
    private func makeInstallationSnapshot() -> InstallationSnapshot {
        let executable = URL(fileURLWithPath: "/Applications/Plug.app/Contents/Resources/plug")
        let app = VerifiedAppInstallation(
            bundleURL: executable.deletingLastPathComponent().deletingLastPathComponent(),
            executableURL: executable,
            appVersion: "0.6.4",
            buildVersion: "12",
            embeddedVersion: "0.6.4",
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
            daemonVersion: "0.6.4",
            daemonExecutable: executable
        )
        return InstallationSnapshot(
            app: app,
            shellLink: .canonical(executable),
            service: service,
            daemonVersion: "0.6.4",
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
          "runtimeVersion": "0.6.4",
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
    private let lock = NSLock()
    private(set) var clientVersion: String?
    private var connection: Int32 = -1

    init(
        events: LockedEvents,
        daemonVersion: String = "0.6.4",
        ipcMin: UInt16 = 3,
        ipcMax: UInt16 = 4
    ) throws {
        self.events = events
        self.daemonVersion = daemonVersion
        self.ipcMin = ipcMin
        self.ipcMax = ipcMax
        socketURL = URL(fileURLWithPath: "/tmp/plug-app-model-\(UUID().uuidString).sock")
        listener = Darwin.socket(AF_UNIX, SOCK_STREAM, 0)
        guard listener >= 0 else { throw PlugIPCError.systemCall("socket", errno) }
        Darwin.unlink(socketURL.path)
        var address = sockaddr_un()
        address.sun_family = sa_family_t(AF_UNIX)
        let bytes = Array(socketURL.path.utf8CString)
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
        let connection = self.connection
        self.connection = -1
        lock.unlock()
        if connection >= 0 { Darwin.close(connection) }
        Darwin.close(listener)
        Darwin.unlink(socketURL.path)
    }

    private func serve() {
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
                        "ownership": "app", "capabilities": [],
                    ],
                ], to: accepted)
            case "OperatorSnapshot":
                events.append("ipc.snapshot")
                send(response: [
                    "type": "OperatorSnapshot",
                    "snapshot": [
                        "runtime_version": "0.6.4", "uptime_secs": 1, "ownership": "app",
                        "configured_servers": [], "servers": [], "live_sessions": [],
                        "client_visibility": [], "upstream_auth": [], "downstream_clients": [],
                    ],
                ], to: accepted)
            case "ActivitySnapshot":
                events.append("ipc.activity")
                send(response: ["type": "ActivitySnapshot", "events": []], to: accepted)
            default:
                send(response: ["type": "Ok"], to: accepted)
            }
        }
        Darwin.close(accepted)
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
