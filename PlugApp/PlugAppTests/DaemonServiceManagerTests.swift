import Darwin
import Foundation
import PlugIPC
import ServiceManagement
import XCTest
@testable import Plug

@MainActor
final class DaemonServiceManagerTests: XCTestCase {
    private let canonical = VerifiedAppInstallation(
        bundleURL: URL(fileURLWithPath: "/Applications/Plug.app"),
        executableURL: URL(fileURLWithPath: "/Applications/Plug.app/Contents/Resources/plug"),
        appVersion: "0.7.0",
        buildVersion: "20",
        embeddedVersion: "0.7.0",
        teamID: "HJF7LN64XX"
    )

    func testFirstLegacyAdoptionRequiresExplicitOperatorAction() async throws {
        let legacy = record(label: "local.claude-rc.plug", path: "/Users/me/.cargo/bin/plug", build: nil)
        let current = record(label: "com.plug.daemon", path: canonical.executableURL.path, build: "20")
        let inspector = SequenceLaunchdInspector([
            .recognizedLegacy([legacy]),
            .recognizedLegacy([legacy]),
            .recognizedLegacy([legacy]),
            .appManagedCurrent(current),
        ])
        let backend = FakeDaemonBackend(handshakes: [handshake("0.6.4"), handshake("0.6.4"), handshake("0.7.0")])
        let manager = makeManager(inspector: inspector, backend: backend)
        let snapshot = try await manager.inspect(canonical: canonical, legacyPaths: [legacy.programURL!])

        do {
            _ = try await manager.ensureRunning(expectedVersion: "0.7.0")
            XCTFail("Expected explicit adoption requirement")
        } catch let error as DaemonServiceError {
            XCTAssertEqual(error, .adoptionRequired)
        }
        XCTAssertFalse(backend.events.contains { if case .bootOut = $0 { true } else { false } })

        let result = try await manager.adoptRecognizedLegacy(snapshot: snapshot, expectedVersion: "0.7.0")
        XCTAssertEqual(result.daemonVersion, "0.7.0")
        XCTAssertEqual(Array(backend.events.suffix(from: 2)), [
            .pause,
            .bootOut(label: legacy.label, path: legacy.programURL!),
            .register,
            .kickstart,
            .sleep,
            .handshake,
            .resume([101, 102]),
        ])
    }

    func testEnsureRunningAutomaticallyReplacesVerifiedStaleAppService() async throws {
        let stale = record(label: "com.plug.daemon", path: canonical.executableURL.path, build: "19")
        let current = record(label: "com.plug.daemon", path: canonical.executableURL.path, build: "20")
        let inspector = SequenceLaunchdInspector([
            .appManagedStale(stale),
            .appManagedStale(stale),
            .appManagedCurrent(current),
        ])
        let backend = FakeDaemonBackend(
            enabled: true,
            handshakes: [handshake("0.6.4"), handshake("0.7.0")]
        )
        let manager = makeManager(inspector: inspector, backend: backend)

        let result = try await manager.ensureRunning(expectedVersion: "0.7.0")

        XCTAssertEqual(result.daemonVersion, "0.7.0")
        XCTAssertEqual(backend.events, [
            .handshake,
            .pause,
            .unregister,
            .register,
            .kickstart,
            .sleep,
            .handshake,
            .resume([101, 102]),
        ])
    }

    func testConnectorsResumeWhenReplacementFails() async throws {
        let stale = record(label: "com.plug.daemon", path: canonical.executableURL.path, build: "19")
        let inspector = SequenceLaunchdInspector([.appManagedStale(stale)])
        let backend = FakeDaemonBackend(enabled: true, handshakes: [handshake("0.6.4")])
        backend.registerError = TestFailure.register
        let manager = makeManager(inspector: inspector, backend: backend)

        do {
            _ = try await manager.replaceStaleAppService(
                snapshot: snapshot(.appManagedStale(stale), version: "0.6.4", executable: stale.programURL),
                expectedVersion: "0.7.0"
            )
            XCTFail("Expected registration failure")
        } catch TestFailure.register {
        } catch {
            XCTFail("Unexpected error: \(error)")
        }

        XCTAssertEqual(backend.events.last, .resume([101, 102]))
        XCTAssertTrue(backend.events.firstIndex(of: .pause)! < backend.events.firstIndex(of: .unregister)!)
        XCTAssertFalse(backend.events.contains { if case .bootOut = $0 { true } else { false } })
    }

    func testWrongVersionReadySocketRetriesAreBoundedThenFail() async throws {
        let current = record(label: "com.plug.daemon", path: canonical.executableURL.path, build: "20")
        let inspector = SequenceLaunchdInspector(Array(repeating: .appManagedCurrent(current), count: 5))
        let backend = FakeDaemonBackend(
            enabled: true,
            handshakes: Array(repeating: handshake("0.6.4"), count: 4)
        )
        let manager = makeManager(inspector: inspector, backend: backend, retryLimit: 3)

        do {
            _ = try await manager.ensureRunning(expectedVersion: "0.7.0")
            XCTFail("Expected version verification failure")
        } catch let error as DaemonServiceError {
            XCTAssertEqual(error, .verificationFailed(expectedVersion: "0.7.0", actualVersion: "0.6.4"))
        }

        XCTAssertEqual(backend.events.filter { $0 == .kickstart }.count, 3)
        XCTAssertEqual(backend.events.last, .resume([101, 102]))
    }

    func testMissingHandshakeExecutableFailsClosedEvenWhenVersionAndBuildMatch() async throws {
        let current = record(label: "com.plug.daemon", path: canonical.executableURL.path, build: "20")
        let inspector = SequenceLaunchdInspector(Array(repeating: .appManagedCurrent(current), count: 3))
        let backend = FakeDaemonBackend(
            enabled: true,
            handshakes: Array(repeating: handshake("0.7.0", executable: nil), count: 2)
        )
        let manager = makeManager(inspector: inspector, backend: backend, retryLimit: 1)

        do {
            _ = try await manager.ensureRunning(expectedVersion: "0.7.0")
            XCTFail("Expected missing executable proof to fail closed")
        } catch let error as DaemonServiceError {
            XCTAssertEqual(error, .verificationFailed(expectedVersion: "0.7.0", actualVersion: "0.7.0"))
        }
    }

    func testSameBuildOtherAppCopyInHandshakeFailsClosed() async throws {
        let current = record(label: "com.plug.daemon", path: canonical.executableURL.path, build: "20")
        let otherAppExecutable = URL(fileURLWithPath: "/Users/me/Applications/Other Plug.app/Contents/Resources/plug")
        let inspector = SequenceLaunchdInspector(Array(repeating: .appManagedCurrent(current), count: 3))
        let backend = FakeDaemonBackend(
            enabled: true,
            handshakes: Array(repeating: handshake("0.7.0", executable: otherAppExecutable), count: 2)
        )
        let manager = makeManager(inspector: inspector, backend: backend, retryLimit: 1)

        do {
            _ = try await manager.ensureRunning(expectedVersion: "0.7.0")
            XCTFail("Expected other app copy proof to fail closed")
        } catch let error as DaemonServiceError {
            XCTAssertEqual(error, .verificationFailed(expectedVersion: "0.7.0", actualVersion: "0.7.0"))
        }
    }

    func testInspectionRetainsHandshakeExecutableForFinalProof() async throws {
        let current = record(label: "com.plug.daemon", path: canonical.executableURL.path, build: "20")
        let handshakeExecutable = URL(fileURLWithPath: "/Users/me/Applications/Other Plug.app/Contents/Resources/plug")
        let inspector = SequenceLaunchdInspector([.appManagedCurrent(current)])
        let backend = FakeDaemonBackend(
            enabled: true,
            handshakes: [handshake("0.7.0", executable: handshakeExecutable)]
        )
        let manager = makeManager(inspector: inspector, backend: backend)

        let snapshot = try await manager.inspect(canonical: canonical, legacyPaths: [])

        XCTAssertEqual(snapshot.daemonExecutable, handshakeExecutable)
    }

    func testUnknownJobIsRefusedWithoutBootoutOrRegistration() async throws {
        let unknown = record(label: "com.plug.daemon", path: "/tmp/not-plug", build: nil)
        let inspector = SequenceLaunchdInspector([.unknown([unknown])])
        let backend = FakeDaemonBackend()
        let manager = makeManager(inspector: inspector, backend: backend)

        do {
            _ = try await manager.ensureRunning(expectedVersion: "0.7.0")
            XCTFail("Expected unknown ownership refusal")
        } catch let error as DaemonServiceError {
            XCTAssertEqual(error, .unknownOwnership)
        }

        XCTAssertTrue(backend.events.isEmpty)
    }

    func testStaleSnapshotMustStillMatchExactLivePathAndBundleEvidence() async throws {
        let claimed = record(label: "com.plug.daemon", path: canonical.executableURL.path, build: "19")
        let unverified = record(label: "com.plug.daemon", path: "/tmp/not-plug", build: nil)
        let inspector = SequenceLaunchdInspector([.unknown([unverified])])
        let backend = FakeDaemonBackend()
        let manager = makeManager(inspector: inspector, backend: backend)

        do {
            _ = try await manager.replaceStaleAppService(
                snapshot: snapshot(.appManagedStale(claimed), version: "0.6.4", executable: claimed.programURL),
                expectedVersion: "0.7.0"
            )
            XCTFail("Expected changed ownership refusal")
        } catch let error as DaemonServiceError {
            XCTAssertEqual(error, .evidenceChanged)
        }

        XCTAssertTrue(backend.events.isEmpty)
    }

    func testAutomaticStaleReplacementRevalidatesOwnerImmediatelyBeforeBootout() async throws {
        let stale = record(label: "com.plug.daemon", path: canonical.executableURL.path, build: "19")
        let changed = record(label: "com.plug.daemon", path: "/tmp/replaced-by-someone-else", build: nil)
        let inspector = SequenceLaunchdInspector([.appManagedStale(stale), .unknown([changed])])
        let backend = FakeDaemonBackend(handshakes: [handshake("0.6.4")])
        let manager = makeManager(inspector: inspector, backend: backend)

        do {
            _ = try await manager.ensureRunning(expectedVersion: "0.7.0")
            XCTFail("Expected changed ownership refusal")
        } catch let error as DaemonServiceError {
            XCTAssertEqual(error, .evidenceChanged)
        }

        XCTAssertEqual(backend.events, [.handshake])
    }

    func testRestartBridgeRevalidatesOwnerImmediatelyBeforeBootout() async throws {
        let current = record(label: "com.plug.daemon", path: canonical.executableURL.path, build: "20")
        let changed = record(label: "com.plug.daemon", path: "/tmp/replaced-by-someone-else", build: nil)
        let inspector = SequenceLaunchdInspector([.appManagedCurrent(current), .unknown([changed])])
        let backend = FakeDaemonBackend(handshakes: [handshake("0.7.0")])
        let manager = makeManager(inspector: inspector, backend: backend)

        do {
            try await manager.restart()
            XCTFail("Expected changed ownership refusal")
        } catch let error as DaemonServiceError {
            XCTAssertEqual(error, .evidenceChanged)
        }

        XCTAssertEqual(backend.events, [.handshake])
    }

    func testTimedOutHandshakeRemainsBoundedByReplacementRetryLimit() async throws {
        let current = record(label: "com.plug.daemon", path: canonical.executableURL.path, build: "20")
        let inspector = SequenceLaunchdInspector(Array(repeating: .appManagedCurrent(current), count: 5))
        let backend = FakeDaemonBackend(
            enabled: true,
            handshakeFailures: Array(
                repeating: PlugIPCError.systemCall("read", ETIMEDOUT),
                count: 4
            )
        )
        let manager = makeManager(inspector: inspector, backend: backend, retryLimit: 3)

        do {
            _ = try await manager.ensureRunning(expectedVersion: "0.7.0")
            XCTFail("Expected bounded verification failure")
        } catch let error as DaemonServiceError {
            XCTAssertEqual(error, .verificationFailed(expectedVersion: "0.7.0", actualVersion: nil))
        }

        XCTAssertEqual(backend.events.filter { $0 == .handshake }.count, 4)
        XCTAssertEqual(backend.events.filter { $0 == .kickstart }.count, 3)
        XCTAssertEqual(backend.events.last, .resume([101, 102]))
    }

    private func makeManager(
        inspector: SequenceLaunchdInspector,
        backend: FakeDaemonBackend,
        retryLimit: Int = 3
    ) -> DaemonServiceManager {
        DaemonServiceManager(
            appInspector: StaticAppInspector(canonical),
            launchdInspector: inspector,
            backend: backend,
            legacyPaths: [],
            retryLimit: retryLimit
        )
    }

    private func record(label: String, path: String, build: String?) -> LaunchdJobRecord {
        LaunchdJobRecord(
            label: label,
            programURL: URL(fileURLWithPath: path),
            parentBundleIdentifier: build == nil ? nil : "com.cyberpapiii.plug",
            parentBundleVersion: build,
            loaded: true
        )
    }

    private func snapshot(
        _ ownership: DaemonOwnershipState,
        version: String?,
        executable: URL?
    ) -> DaemonServiceSnapshot {
        DaemonServiceSnapshot(ownership: ownership, daemonVersion: version, daemonExecutable: executable)
    }
}

private enum TestFailure: Error {
    case register
}

private struct StaticAppInspector: AppInstallationInspecting {
    let installation: VerifiedAppInstallation

    init(_ installation: VerifiedAppInstallation) {
        self.installation = installation
    }

    func inspectCurrentApp() async throws -> VerifiedAppInstallation { installation }
}

private actor SequenceLaunchdInspector: LaunchdJobInspecting {
    private var states: [DaemonOwnershipState]

    init(_ states: [DaemonOwnershipState]) {
        self.states = states
    }

    func daemonJobs(
        canonical: VerifiedAppInstallation,
        recognizedLegacyPaths: Set<URL>
    ) async throws -> DaemonOwnershipState {
        guard !states.isEmpty else { throw CocoaError(.fileReadUnknown) }
        if states.count == 1 { return states[0] }
        return states.removeFirst()
    }
}

@MainActor
private final class FakeDaemonBackend: DaemonServiceBackend {
    enum Event: Equatable {
        case handshake
        case pause
        case bootOut(label: String, path: URL)
        case unregister
        case register
        case kickstart
        case sleep
        case resume([Int32])
    }

    var enabled: Bool
    var serviceStatus: SMAppService.Status { enabled ? .enabled : .notRegistered }
    var events: [Event] = []
    var registerError: Error?
    private var handshakes: [OperatorHandshake]
    private var handshakeFailures: [Error]

    init(
        enabled: Bool = false,
        handshakes: [OperatorHandshake] = [],
        handshakeFailures: [Error] = []
    ) {
        self.enabled = enabled
        self.handshakes = handshakes
        self.handshakeFailures = handshakeFailures
    }

    func pauseConnectors() -> [Int32] {
        events.append(.pause)
        return [101, 102]
    }

    func resumeConnectors(_ pids: [Int32]) {
        events.append(.resume(pids))
    }

    func bootOut(_ record: LaunchdJobRecord) async throws {
        events.append(.bootOut(label: record.label, path: record.programURL!))
    }

    func unregisterAgent() async throws {
        events.append(.unregister)
        enabled = false
    }

    func registerAgent() throws {
        events.append(.register)
        if let registerError { throw registerError }
        enabled = true
    }

    func kickstartAgent() async throws {
        events.append(.kickstart)
    }

    func handshake() async throws -> OperatorHandshake {
        events.append(.handshake)
        if !handshakeFailures.isEmpty { throw handshakeFailures.removeFirst() }
        guard !handshakes.isEmpty else { throw CocoaError(.fileReadUnknown) }
        if handshakes.count == 1 { return handshakes[0] }
        return handshakes.removeFirst()
    }

    func waitBeforeRetry() async {
        events.append(.sleep)
    }

    func openLoginItemSettings() {}
}

private func handshake(_ version: String, executable: URL? = URL(fileURLWithPath: "/Applications/Plug.app/Contents/Resources/plug")) -> OperatorHandshake {
    let executableJSON = executable.map { "\"daemonExecutable\":\"\($0.path)\", " } ?? ""
    let json = """
    {"daemonVersion":"\(version)",\(executableJSON)"ipcMin":3,"ipcMax":4,"ownership":"app","capabilities":[]}
    """
    return try! JSONDecoder().decode(OperatorHandshake.self, from: Data(json.utf8))
}
