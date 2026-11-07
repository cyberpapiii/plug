import Darwin
import Foundation
import PlugIPC
import ServiceManagement
import XCTest
@testable import Plug

/// The manager against a pretend launchd and daemon that change the way the
/// real ones do: registering lists the app's job, unregistering or booting a
/// job out stops whatever it ran, and a started agent answers after a few
/// waits. Tests set up a world and check where it ends up.
@MainActor
final class DaemonServiceManagerTests: XCTestCase {
    private let canonical = makeTestInstallation()
    private lazy var current = record(label: "com.plug.daemon", path: canonical.executableURL.path, build: "20")
    private lazy var stale = record(label: "com.plug.daemon", path: canonical.executableURL.path, build: "19")
    private let cellar = LaunchdJobRecord(
        label: "com.plug.daemon",
        programURL: URL(fileURLWithPath: "/opt/homebrew/Cellar/plug/0.6.3/bin/plug"),
        parentBundleIdentifier: nil,
        parentBundleVersion: nil
    )
    private lazy var ours = makeTestHandshake(version: "0.7.0", executable: canonical.executableURL)

    /// A started daemon is checked for as long as the CLI waits for one,
    /// the budget `testdata/paths.json` holds both to.
    func testAStartIsWaitedForAsLongAsTheCLIWaits() {
        XCTAssertEqual(
            DaemonServiceManager.retryInterval * DaemonServiceManager.startupRetries,
            PlugPaths.startupBudget
        )
    }

    func testStopUnregistersOwnedServiceWithoutStartingOrSuspendingClients() async throws {
        let launchd = world(.appManagedCurrent(current), enabled: true, answering: ours)

        try await manager(launchd).stop()

        XCTAssertFalse(launchd.enabled)
        XCTAssertEqual(launchd.events, [.unregister])
    }

    func testStopDoesNotUnregisterUnknownService() async throws {
        let launchd = world(.unknown([]), enabled: true)

        await assertFails(with: .unknownOwnership) { try await self.manager(launchd).stop() }

        XCTAssertTrue(launchd.enabled)
        XCTAssertTrue(launchd.events.isEmpty)
    }

    /// Before the formula is uninstalled, its jobs go, and only its jobs: a
    /// Cargo leftover keeps running until the app's own daemon is proven.
    func testOnlyHomebrewLeftoversAreBootedOutBeforeTheFormulaGoes() async throws {
        let cargo = record(label: "local.claude-rc.plug", path: "/Users/me/.cargo/bin/plug", build: nil)
        let brewBin = record(label: "homebrew.mxcl.plug", path: "/opt/homebrew/bin/plug", build: nil)
        let leftovers = DaemonOwnershipState.recognizedLegacy([cellar, cargo, brewBin])
        let launchd = world(leftovers)

        try await manager(launchd).bootOutRecognizedLegacy(snapshot(leftovers, version: nil, executable: nil))

        XCTAssertEqual(launchd.events, [
            .bootOut(label: cellar.label, path: cellar.programURL!),
            .bootOut(label: brewBin.label, path: brewBin.programURL!),
        ])
    }

    func testNothingIsBootedOutWithoutAHomebrewLeftover() async throws {
        let cargo = record(label: "local.claude-rc.plug", path: "/Users/me/.cargo/bin/plug", build: nil)
        let unread = LaunchdJobRecord(
            label: "com.plug.serve", programURL: nil, parentBundleIdentifier: nil, parentBundleVersion: nil
        )
        for ownership in [
            DaemonOwnershipState.recognizedLegacy([cargo, unread]), .appManagedCurrent(current),
            .appManagedStale(stale), .unmanaged, .unknown([cellar]),
        ] {
            let launchd = world(ownership)
            try await manager(launchd).bootOutRecognizedLegacy(snapshot(ownership, version: nil, executable: nil))
            XCTAssertTrue(launchd.events.isEmpty, "\(ownership)")
        }
    }

    /// A failed press shows these as they are.
    func testEveryServiceFailureReadsAsASentence() {
        let errors: [DaemonServiceError] = [
            .adoptionRequired, .unknownOwnership, .evidenceChanged,
            .invalidAppVersion(expected: "0.9.1", actual: "0.9.0"), .invalidJobEvidence, .registrationDisabled,
            .verificationFailed(expectedVersion: "0.9.1", actualVersion: nil),
            .verificationFailed(expectedVersion: "0.9.1", actualVersion: "0.9.0"),
            .commandFailed(""), .commandFailed("Plug could not turn off. Try again."),
        ]
        for error in errors {
            let text = (error as Error).localizedDescription
            XCTAssertEqual(text, error.errorDescription, "\(error)")
            XCTAssertTrue(text.hasSuffix("."), text)
        }
        XCTAssertEqual(
            (DaemonServiceError.commandFailed("Plug could not turn off. Try again.") as Error).localizedDescription,
            "Plug could not turn off. Try again."
        )
    }

    func testAdoptingALeftoverReadsLaunchdWithTheProgramItFound() async throws {
        let launchd = world(.recognizedLegacy([cellar]), answering: makeTestHandshake(version: "0.6.3"))
        let manager = manager(launchd)
        let snapshot = try await manager.inspect(canonical: canonical, legacyPaths: [])

        let result = try await manager.adopt(snapshot: snapshot, expectedVersion: "0.7.0")

        XCTAssertEqual(result.daemonVersion, "0.7.0")
        XCTAssertEqual(launchd.ownership, .appManagedCurrent(current))
        XCTAssertTrue(
            launchd.recognizedPathSets.contains { $0.contains(cellar.programURL!) },
            "the job is read again with the Cellar program the first look found, not only the default paths"
        )
    }

    func testAdoptingALeftoverThatIsAlreadyGoneRegistersWithoutABootOut() async throws {
        let launchd = world(.recognizedLegacy([cellar]), answering: makeTestHandshake(version: "0.6.3"))
        launchd.changedAfterFirstLook = .unmanaged
        let manager = manager(launchd)
        let snapshot = try await manager.inspect(canonical: canonical, legacyPaths: [])

        let result = try await manager.adopt(snapshot: snapshot, expectedVersion: "0.7.0")

        XCTAssertEqual(result.daemonVersion, "0.7.0")
        XCTAssertFalse(launchd.bootedOut)
        XCTAssertTrue(launchd.enabled)
    }

    func testALeftoverIsTakenOverOnlyWhenAskedTo() async throws {
        let legacy = record(label: "local.claude-rc.plug", path: "/Users/me/.cargo/bin/plug", build: nil)
        let launchd = world(.recognizedLegacy([legacy]), answering: makeTestHandshake(version: "0.6.4"))
        let manager = manager(launchd)
        let snapshot = try await manager.inspect(canonical: canonical, legacyPaths: [legacy.programURL!])

        await assertFails(with: .adoptionRequired) {
            _ = try await manager.ensureRunning(canonical: self.canonical, inspected: snapshot)
        }
        XCTAssertTrue(launchd.events.isEmpty)

        let result = try await manager.adopt(snapshot: snapshot, expectedVersion: "0.7.0")
        XCTAssertEqual(result.daemonVersion, "0.7.0")
        XCTAssertEqual(launchd.events, [
            .pause,
            .bootOut(label: legacy.label, path: legacy.programURL!),
            .register,
            .kickstart,
            .resume([101, 102]),
        ])
    }

    func testADaemonAnsweringWithNoRegistrationIsReplacedWhenAdopted() async throws {
        let launchd = world(.unmanaged, answering: makeTestHandshake(version: "0.6.4"))
        let manager = manager(launchd)
        let snapshot = try await manager.inspect(canonical: canonical, legacyPaths: [])

        let result = try await manager.adopt(snapshot: snapshot, expectedVersion: "0.7.0")

        XCTAssertEqual(result.daemonVersion, "0.7.0")
        XCTAssertEqual(launchd.ownership, .appManagedCurrent(current))
        XCTAssertFalse(launchd.bootedOut)
    }

    /// SMAppService has to forget the old registration before the new one is
    /// made, and the connectors wait out the swap.
    func testAStaleAppServiceIsReplacedWithoutBeingAsked() async throws {
        let launchd = world(.appManagedStale(stale), enabled: true, answering: makeTestHandshake(version: "0.6.4"))
        let manager = manager(launchd)
        let snapshot = try await manager.inspect(canonical: canonical, legacyPaths: [])

        let result = try await manager.ensureRunning(canonical: canonical, inspected: snapshot)

        XCTAssertEqual(result.daemonVersion, "0.7.0")
        XCTAssertEqual(launchd.events, [.pause, .unregister, .register, .kickstart, .resume([101, 102])])
    }

    func testARegistrationThatFailsOnceIsTriedAgain() async throws {
        let launchd = world(.appManagedStale(stale), enabled: true, answering: makeTestHandshake(version: "0.6.4"))
        launchd.registerFailuresLeft = 1

        let result = try await manager(launchd).replaceStaleAppService(
            snapshot: snapshot(.appManagedStale(stale), version: "0.6.4", executable: stale.programURL),
            expectedVersion: "0.7.0"
        )

        XCTAssertEqual(result.daemonVersion, "0.7.0")
        XCTAssertEqual(launchd.events.filter { $0 == .register }.count, 2)
    }

    func testConnectorsResumeWhenReplacementFails() async throws {
        let launchd = world(.appManagedStale(stale), enabled: true, answering: makeTestHandshake(version: "0.6.4"))
        launchd.registerError = TestFailure.register

        do {
            _ = try await manager(launchd).replaceStaleAppService(
                snapshot: snapshot(.appManagedStale(stale), version: "0.6.4", executable: stale.programURL),
                expectedVersion: "0.7.0"
            )
            XCTFail("Expected registration failure")
        } catch TestFailure.register {
        } catch {
            XCTFail("Unexpected error: \(error)")
        }

        XCTAssertEqual(launchd.events.last, .resume([101, 102]))
        XCTAssertTrue(launchd.events.firstIndex(of: .pause)! < launchd.events.firstIndex(of: .unregister)!)
        XCTAssertFalse(launchd.bootedOut)
    }

    /// A kickstart while a cold daemon is still starting its servers kills
    /// it, so the replacement starts it once and only waits after that.
    func testADaemonThatKeepsAnsweringWithTheWrongVersionFailsAfterOneStart() async throws {
        let old = makeTestHandshake(version: "0.6.4")
        let launchd = world(.appManagedCurrent(current), enabled: true, answering: old, started: old)

        await assertFails(with: .verificationFailed(expectedVersion: "0.7.0", actualVersion: "0.6.4")) {
            _ = try await self.manager(launchd).ensureRunning(canonical: self.canonical, inspected: self.inspected())
        }

        XCTAssertEqual(launchd.events.filter { $0 == .kickstart }.count, 1)
        XCTAssertEqual(launchd.events.last, .resume([101, 102]))
    }

    func testAHandshakeWithNoExecutableIsNotProof() async throws {
        let unproven = makeTestHandshake(version: "0.7.0", executable: nil)
        let launchd = world(.appManagedCurrent(current), enabled: true, answering: unproven, started: unproven)

        await assertFails(with: .verificationFailed(expectedVersion: "0.7.0", actualVersion: "0.7.0")) {
            _ = try await self.manager(launchd, retryLimit: 1)
                .ensureRunning(canonical: self.canonical, inspected: self.inspected())
        }
    }

    /// Reconcile read launchd and verified the app a moment ago. One exact
    /// handshake is enough to prove the service is ours and current.
    func testAFreshInspectionAndAnExactHandshakeNeedNothingElse() async throws {
        let launchd = world(.appManagedCurrent(current), enabled: true, answering: ours)

        let proven = try await manager(launchd).ensureRunning(canonical: canonical, inspected: inspected())

        XCTAssertEqual(proven.daemonVersion, "0.7.0")
        XCTAssertTrue(launchd.events.isEmpty)
        XCTAssertTrue(launchd.recognizedPathSets.isEmpty, "a fresh inspection needs no second launchd read")
    }

    /// Same version and build, but another copy of the app on this Mac.
    func testADaemonFromAnotherCopyOfTheAppIsNotProof() async throws {
        let other = makeTestHandshake(
            version: "0.7.0",
            executable: URL(fileURLWithPath: "/Users/me/Applications/Other Plug.app/Contents/Resources/plug")
        )
        let launchd = world(.appManagedCurrent(current), enabled: true, answering: other, started: other)

        await assertFails(with: .verificationFailed(expectedVersion: "0.7.0", actualVersion: "0.7.0")) {
            _ = try await self.manager(launchd, retryLimit: 1)
                .ensureRunning(canonical: self.canonical, inspected: self.inspected())
        }
        XCTAssertFalse(launchd.recognizedPathSets.isEmpty, "a disagreeing handshake reads launchd again")
    }

    func testInspectionRetainsHandshakeExecutableForFinalProof() async throws {
        let executable = URL(fileURLWithPath: "/Users/me/Applications/Other Plug.app/Contents/Resources/plug")
        let launchd = world(
            .appManagedCurrent(current),
            enabled: true,
            answering: makeTestHandshake(version: "0.7.0", executable: executable)
        )

        let snapshot = try await manager(launchd).inspect(canonical: canonical, legacyPaths: [])

        XCTAssertEqual(snapshot.daemonExecutable, executable)
    }

    func testConnectorsAnEarlierAppLeftStoppedAreWokenOnce() async throws {
        let launchd = world(.appManagedCurrent(current), enabled: true, answering: ours)
        let manager = manager(launchd)

        _ = try await manager.inspect(canonical: canonical, legacyPaths: [])
        _ = try await manager.inspect(canonical: canonical, legacyPaths: [])

        XCTAssertEqual(launchd.strandedResumes, 1)
    }

    func testUnknownJobIsRefusedWithoutBootoutOrRegistration() async throws {
        let unknown = record(label: "com.plug.daemon", path: "/tmp/not-plug", build: nil)
        let launchd = world(.unknown([unknown]))
        let manager = manager(launchd)
        let snapshot = try await manager.inspect(canonical: canonical, legacyPaths: [])

        await assertFails(with: .unknownOwnership) {
            _ = try await manager.ensureRunning(canonical: self.canonical, inspected: snapshot)
        }

        XCTAssertTrue(launchd.events.isEmpty)
    }

    func testStaleSnapshotMustStillMatchExactLivePathAndBundleEvidence() async throws {
        let unverified = record(label: "com.plug.daemon", path: "/tmp/not-plug", build: nil)
        let launchd = world(.unknown([unverified]))

        await assertFails(with: .evidenceChanged) {
            _ = try await self.manager(launchd).replaceStaleAppService(
                snapshot: self.snapshot(.appManagedStale(self.stale), version: "0.6.4", executable: self.stale.programURL),
                expectedVersion: "0.7.0"
            )
        }

        XCTAssertTrue(launchd.events.isEmpty)
    }

    func testAStaleJobSomeoneElseReplacesIsNotBootedOut() async throws {
        let changed = record(label: "com.plug.daemon", path: "/tmp/replaced-by-someone-else", build: nil)
        let launchd = world(.appManagedStale(stale), answering: makeTestHandshake(version: "0.6.4"))
        launchd.changedAfterFirstLook = .unknown([changed])

        await assertFails(with: .evidenceChanged) {
            _ = try await self.manager(launchd).ensureRunning(
                canonical: self.canonical,
                inspected: self.snapshot(.appManagedStale(self.stale), version: "0.6.4", executable: self.stale.programURL)
            )
        }

        XCTAssertTrue(launchd.events.isEmpty)
    }

    func testRestartRereadsTheOwnerImmediatelyBeforeBootout() async throws {
        let changed = record(label: "com.plug.daemon", path: "/tmp/replaced-by-someone-else", build: nil)
        let launchd = world(.appManagedCurrent(current), answering: ours)
        launchd.changedAfterFirstLook = .unknown([changed])

        await assertFails(with: .evidenceChanged) { try await self.manager(launchd).restart() }

        XCTAssertTrue(launchd.events.isEmpty)
    }

    /// One look, one window for the silent daemon, then one replacement with
    /// a window of its own.
    func testADaemonThatNeverAnswersIsReplacedOnceThenReported() async throws {
        let launchd = world(.appManagedCurrent(current), enabled: true)
        launchd.silence = PlugIPCError.systemCall("read", ETIMEDOUT)
        launchd.neverStarts = true

        await assertFails(with: .verificationFailed(expectedVersion: "0.7.0", actualVersion: nil)) {
            _ = try await self.manager(launchd).ensureRunning(canonical: self.canonical, inspected: self.inspected())
        }

        XCTAssertEqual(launchd.events.filter { $0 == .unregister }.count, 1)
        XCTAssertEqual(launchd.events.last, .resume([101, 102]))
    }

    func testADaemonStillStartingIsWaitedForAndNotReplaced() async throws {
        let launchd = world(.appManagedCurrent(current), enabled: true, started: ours)
        launchd.startupWaits = 2

        let result = try await manager(launchd).ensureRunning(canonical: canonical, inspected: inspected())

        XCTAssertEqual(result.daemonVersion, "0.7.0")
        XCTAssertFalse(launchd.events.contains(.unregister), "a daemon that is only slow is left alone")
    }

    // MARK: - Helpers

    /// `registered` is what launchd lists once the app registers its agent,
    /// and `started` what answers once that agent starts.
    private func world(
        _ ownership: DaemonOwnershipState,
        enabled: Bool = false,
        answering: OperatorHandshake? = nil,
        started: OperatorHandshake? = nil
    ) -> FakeLaunchd {
        FakeLaunchd(
            ownership: ownership,
            registered: .appManagedCurrent(current),
            enabled: enabled,
            answering: answering,
            started: started ?? ours
        )
    }

    private func manager(_ launchd: FakeLaunchd, retryLimit: Int = 3) -> DaemonServiceManager {
        DaemonServiceManager(
            appInspector: StaticAppInspector(canonical),
            launchdInspector: FakeLaunchdInspector(launchd: launchd),
            backend: launchd,
            legacyPaths: [],
            retryLimit: retryLimit
        )
    }

    /// The snapshot reconcile hands over after reading the app's current job.
    private func inspected() -> DaemonServiceSnapshot {
        snapshot(.appManagedCurrent(current), version: "0.7.0", executable: canonical.executableURL)
    }

    private func record(label: String, path: String, build: String?) -> LaunchdJobRecord {
        LaunchdJobRecord(
            label: label,
            programURL: URL(fileURLWithPath: path),
            parentBundleIdentifier: build == nil ? nil : "com.cyberpapiii.plug",
            parentBundleVersion: build
        )
    }

    private func snapshot(
        _ ownership: DaemonOwnershipState,
        version: String?,
        executable: URL?
    ) -> DaemonServiceSnapshot {
        DaemonServiceSnapshot(ownership: ownership, daemonVersion: version, daemonExecutable: executable)
    }

    private func assertFails(
        with expected: DaemonServiceError,
        file: StaticString = #filePath,
        line: UInt = #line,
        _ operation: () async throws -> Void
    ) async {
        do {
            try await operation()
            XCTFail("Expected \(expected)", file: file, line: line)
        } catch let error as DaemonServiceError {
            XCTAssertEqual(error, expected, file: file, line: line)
        } catch {
            XCTFail("Unexpected error: \(error)", file: file, line: line)
        }
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

private struct FakeLaunchdInspector: LaunchdJobInspecting {
    let launchd: FakeLaunchd

    func daemonJobs(
        canonical: VerifiedAppInstallation,
        recognizedLegacyPaths: Set<URL>
    ) async throws -> DaemonOwnershipState {
        await launchd.read(recognizedLegacyPaths)
    }
}

/// launchd, SMAppService and the daemon's socket, as far as the manager can
/// see them. Only what changes the system is recorded as an event.
@MainActor
private final class FakeLaunchd: DaemonServiceBackend {
    enum Event: Equatable {
        case pause
        case bootOut(label: String, path: URL)
        case unregister
        case register
        case kickstart
        case resume([Int32])
    }

    /// What `launchctl` lists now.
    private(set) var ownership: DaemonOwnershipState
    private let registered: DaemonOwnershipState
    /// What a later read finds instead: someone else changed the job after
    /// the first look.
    var changedAfterFirstLook: DaemonOwnershipState?
    private(set) var enabled: Bool
    /// The daemon on the socket now; nil when nothing answers.
    private var answering: OperatorHandshake?
    /// What answers once the agent starts, after `startupWaits` waits.
    private let started: OperatorHandshake
    var startupWaits = 0
    /// The agent starts and nothing ever answers.
    var neverStarts = false
    private var waitsUntilStarted: Int?
    /// What a handshake fails with while nothing answers.
    var silence: Error = PlugIPCError.systemCall("connect", ECONNREFUSED)
    var registerError: Error?
    var registerFailuresLeft = 0

    private(set) var events: [Event] = []
    private(set) var recognizedPathSets: [Set<URL>] = []
    private(set) var strandedResumes = 0

    var bootedOut: Bool { events.contains { if case .bootOut = $0 { true } else { false } } }

    init(
        ownership: DaemonOwnershipState,
        registered: DaemonOwnershipState,
        enabled: Bool,
        answering: OperatorHandshake?,
        started: OperatorHandshake
    ) {
        self.ownership = ownership
        self.registered = registered
        self.enabled = enabled
        self.answering = answering
        self.started = started
    }

    func read(_ paths: Set<URL>) -> DaemonOwnershipState {
        recognizedPathSets.append(paths)
        let seen = ownership
        if let changed = changedAfterFirstLook {
            ownership = changed
            changedAfterFirstLook = nil
        }
        return seen
    }

    func pauseConnectors() async -> [Int32] {
        events.append(.pause)
        return [101, 102]
    }

    func resumeConnectors(_ pids: [Int32]) {
        events.append(.resume(pids))
    }

    func resumeStrandedConnectors() async {
        strandedResumes += 1
    }

    func bootOut(_ record: LaunchdJobRecord) async throws {
        events.append(.bootOut(label: record.label, path: record.programURL!))
        ownership = .unmanaged
        answering = nil
    }

    func unregisterAgent() async throws {
        events.append(.unregister)
        enabled = false
        if case .appManagedCurrent = ownership { ownership = .unmanaged }
        if case .appManagedStale = ownership { ownership = .unmanaged }
        answering = nil
    }

    func registerAgent() throws {
        events.append(.register)
        if registerFailuresLeft > 0 {
            registerFailuresLeft -= 1
            throw TestFailure.register
        }
        if let registerError { throw registerError }
        enabled = true
        ownership = registered
    }

    func kickstartAgent() async throws {
        events.append(.kickstart)
        guard enabled else { return }
        answering = nil
        waitsUntilStarted = startupWaits
    }

    func handshake() async throws -> OperatorHandshake {
        guard let answering else { throw silence }
        return answering
    }

    func waitBeforeRetry() async {
        guard let waits = waitsUntilStarted else { return }
        if waits == 0 {
            if neverStarts { return }
            answering = started
            waitsUntilStarted = nil
        } else {
            waitsUntilStarted = waits - 1
        }
    }

    func openLoginItemSettings() {}
}
