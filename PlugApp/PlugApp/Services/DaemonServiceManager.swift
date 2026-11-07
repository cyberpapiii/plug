import Darwin
import Foundation
import PlugIPC
import ServiceManagement

enum DaemonServiceError: LocalizedError, AdvisedError, Equatable {
    case adoptionRequired
    case unknownOwnership
    case evidenceChanged
    case invalidAppVersion(expected: String, actual: String)
    case invalidJobEvidence
    case registrationDisabled
    case verificationFailed(expectedVersion: String, actualVersion: String?)
    case commandFailed(String)

    var errorDescription: String? {
        switch self {
        case .adoptionRequired:
            "An older copy of Plug is running. Open Plug and let it replace that copy."
        case .unknownOwnership:
            "Something other than this app runs Plug's service, so Plug left it alone."
        case .evidenceChanged:
            "Plug's service changed while Plug was checking it. Try again."
        case let .invalidAppVersion(expected, actual):
            "This copy of Plug is version \(actual), and \(expected) was asked for. Quit Plug and open it again."
        case .invalidJobEvidence:
            "Plug could not tell which program a launchd job runs, so it left the job alone."
        case .registrationDisabled:
            "macOS has turned off Plug's service. Turn Plug on in System Settings, under General, Login Items."
        case let .verificationFailed(expected, actual?):
            "Plug's service answered as version \(actual) instead of \(expected). Try again."
        case .verificationFailed:
            "Plug's service did not answer after it started. Try again."
        case let .commandFailed(detail):
            detail.isEmpty ? "macOS did not change Plug's service. Try again." : detail
        }
    }

    var advice: String {
        if case let .commandFailed(detail) = self { return Explain.advice(forOutside: detail) }
        return Explain.fallback
    }
}

@MainActor
protocol DaemonServiceBackend: AnyObject {
    var enabled: Bool { get }
    func pauseConnectors() async -> [Int32]
    func resumeConnectors(_ pids: [Int32])
    func resumeStrandedConnectors() async
    func bootOut(_ record: LaunchdJobRecord) async throws
    func unregisterAgent() async throws
    func registerAgent() throws
    func kickstartAgent() async throws
    func handshake() async throws -> OperatorHandshake
    /// The time a wait is measured against, so tests can move it.
    var now: ContinuousClock.Instant { get }
    func waitBeforeRetry() async
    func openLoginItemSettings()
}

@MainActor
final class DaemonServiceManager {
    static let shared = DaemonServiceManager()

    private let appInspector: any AppInstallationInspecting
    private let launchdInspector: any LaunchdJobInspecting
    private let backend: any DaemonServiceBackend
    private let legacyPaths: Set<URL>
    private let startupBudget: Duration
    private var resumedStrandedConnectors = false

    init(
        appInspector: any AppInstallationInspecting = AppInstallationInspector(),
        launchdInspector: any LaunchdJobInspecting = LaunchdJobInspector(),
        backend: any DaemonServiceBackend = SystemDaemonServiceBackend(),
        legacyPaths: Set<URL> = DaemonServiceManager.defaultLegacyPaths,
        startupBudget: Duration = PlugPaths.startupBudget
    ) {
        self.appInspector = appInspector
        self.launchdInspector = launchdInspector
        self.backend = backend
        self.legacyPaths = legacyPaths
        self.startupBudget = startupBudget
    }

    /// The wait between two checks of a starting daemon.
    nonisolated static let retryInterval: Duration = .milliseconds(500)

    /// The name the daemon's LaunchAgent is registered under, which is also
    /// its launchd label with `.plist` added.
    nonisolated static let agentPlistName = "\(PlugPaths.daemonLabel).plist"
    nonisolated static let daemonLabel = PlugPaths.daemonLabel

    var appServiceEnabled: Bool { backend.enabled }

    func inspect(
        canonical: VerifiedAppInstallation,
        legacyPaths: Set<URL>
    ) async throws -> DaemonServiceSnapshot {
        if !resumedStrandedConnectors {
            // An app that died in the middle of a swap left every connector
            // stopped, and nothing else would ever wake them.
            resumedStrandedConnectors = true
            await backend.resumeStrandedConnectors()
        }
        return try await inspectWithHandshake(canonical: canonical, legacyPaths: legacyPaths).snapshot
    }

    func bootOutRecognizedLegacy(_ snapshot: DaemonServiceSnapshot) async throws {
        guard case let .recognizedLegacy(records) = snapshot.ownership else { return }
        let homebrew = records.filter { record in
            record.programURL.map(LegacyPlugProgram.isHomebrew) == true
        }
        for record in homebrew {
            try await backend.bootOut(record)
        }
    }

    /// Takes over a daemon this app does not own, with the owner's consent:
    /// a leftover install's job, or a socket with no registration behind it.
    func adopt(
        snapshot: DaemonServiceSnapshot,
        expectedVersion: String
    ) async throws -> OperatorHandshake {
        let records: [LaunchdJobRecord]
        switch snapshot.ownership {
        case let .recognizedLegacy(found) where !found.isEmpty:
            records = found
        case .unmanaged:
            records = []
        default:
            throw DaemonServiceError.adoptionRequired
        }
        let canonical = try await verifiedApp(expectedVersion: expectedVersion)
        let inspectionPaths = inspectionPaths(including: snapshot)
        let current = try await launchdInspector.daemonJobs(
            canonical: canonical,
            recognizedLegacyPaths: inspectionPaths
        )
        let bootOutLoadedJobs: Bool
        switch current {
        case .unmanaged:
            // Formula uninstall or a prior bootout may have already dropped
            // the leftover job. Register from the inspect-time snapshot.
            bootOutLoadedJobs = false
        case .recognizedLegacy:
            guard !records.isEmpty else { throw DaemonServiceError.evidenceChanged }
            bootOutLoadedJobs = true
        case .unknown, .appManagedCurrent, .appManagedStale:
            throw DaemonServiceError.unknownOwnership
        }
        return try await replace(
            verifiedRecords: records,
            canonical: canonical,
            expectedVersion: expectedVersion,
            replacingAppRegistration: false,
            inspectionPaths: inspectionPaths,
            bootOutLoadedJobs: bootOutLoadedJobs
        )
    }

    func replaceStaleAppService(
        snapshot: DaemonServiceSnapshot,
        expectedVersion: String
    ) async throws -> OperatorHandshake {
        guard case let .appManagedStale(record) = snapshot.ownership else {
            throw DaemonServiceError.evidenceChanged
        }
        let canonical = try await verifiedApp(expectedVersion: expectedVersion)
        return try await replaceAppOwned(
            snapshot: snapshot,
            record: record,
            canonical: canonical,
            expectedVersion: expectedVersion
        )
    }

    /// Proves this app's own daemon is running, starting or replacing it
    /// when it is not. `inspected` is the caller's launchd read from moments
    /// ago in the same pass: verifying the app signs and runs the bundled
    /// binary, and a launchd read runs `launchctl` once per job, so repeating
    /// both was most of what a healthy launch spent. When the inspected job is
    /// ours, one fresh handshake proves the daemon; anything short of exact
    /// proof reads launchd again before it changes anything.
    func ensureRunning(
        canonical: VerifiedAppInstallation,
        inspected: DaemonServiceSnapshot
    ) async throws -> OperatorHandshake {
        let expectedVersion = canonical.appVersion
        if case let .appManagedCurrent(record) = inspected.ownership,
           let handshake = try? await backend.handshake(),
           canonical.isOwnCurrentDaemon(record, version: handshake.daemonVersion, executable: handshake.daemonExecutable)
        {
            return handshake
        }

        let inspection = try await inspectWithHandshake(
            canonical: canonical,
            legacyPaths: legacyPaths
        )
        switch inspection.snapshot.ownership {
        case let .appManagedCurrent(record):
            if let handshake = inspection.handshake,
               canonical.isOwnCurrentDaemon(record, version: handshake.daemonVersion, executable: handshake.daemonExecutable)
            {
                return handshake
            }
            // The registration is right and nothing answered: a daemon still
            // starting its servers has no socket yet, and one that stopped
            // only needs starting. Replacing it here would kill the first and
            // is more than the second needs.
            if inspection.handshake == nil {
                try? await backend.kickstartAgent()
                let (proven, _) = try await awaitProof(
                    canonical: canonical,
                    expectedVersion: expectedVersion,
                    inspectionPaths: legacyPaths
                )
                if let proven { return proven }
            }
            return try await replaceAppOwned(
                snapshot: inspection.snapshot,
                record: record,
                canonical: canonical,
                expectedVersion: expectedVersion
            )
        case let .appManagedStale(record):
            return try await replaceAppOwned(
                snapshot: inspection.snapshot,
                record: record,
                canonical: canonical,
                expectedVersion: expectedVersion
            )
        case .recognizedLegacy, .unmanaged:
            throw DaemonServiceError.adoptionRequired
        case .unknown:
            throw DaemonServiceError.unknownOwnership
        }
    }

    /// Replaces this app's own daemon with a fresh one. Only a job read again
    /// now and proved to be the app's own is booted out.
    func restart() async throws {
        let canonical = try await appInspector.inspectCurrentApp()
        let snapshot = try await inspect(canonical: canonical, legacyPaths: legacyPaths)
        switch snapshot.ownership {
        case let .appManagedCurrent(record), let .appManagedStale(record):
            _ = try await replaceAppOwned(
                snapshot: snapshot,
                record: record,
                canonical: canonical,
                expectedVersion: canonical.appVersion
            )
        case .recognizedLegacy, .unmanaged:
            throw DaemonServiceError.adoptionRequired
        case .unknown:
            throw DaemonServiceError.unknownOwnership
        }
    }

    /// Stop only this app's verified agent. Quit remains unrelated to service
    /// ownership; connectors are never left suspended while Plug is off.
    func stop() async throws {
        let canonical = try await appInspector.inspectCurrentApp()
        let ownership = try await launchdInspector.daemonJobs(
            canonical: canonical, recognizedLegacyPaths: legacyPaths
        )
        switch ownership {
        case .appManagedCurrent, .appManagedStale:
            let current = try await launchdInspector.daemonJobs(
                canonical: canonical, recognizedLegacyPaths: legacyPaths
            )
            guard current == ownership else { throw DaemonServiceError.evidenceChanged }
            try await backend.unregisterAgent()
            guard !backend.enabled else { throw DaemonServiceError.commandFailed("Plug could not turn off. Try again.") }
        case .unmanaged where !backend.enabled:
            return
        case .recognizedLegacy, .unmanaged:
            throw DaemonServiceError.adoptionRequired
        case .unknown:
            throw DaemonServiceError.unknownOwnership
        }
    }

    func setMainAppAtLogin(_ enabled: Bool) throws {
        let service = SMAppService.mainApp
        if enabled, service.status != .enabled { try service.register() }
        if !enabled, service.status == .enabled { try service.unregister() }
    }

    func openLoginItemSettings() { backend.openLoginItemSettings() }

    static func connectorPIDs(psOutput: String) -> [Int32] {
        psOutput.split(separator: "\n").compactMap { row in
            let fields = row.split(whereSeparator: \.isWhitespace)
            guard fields.count >= 3,
                  let pid = Int32(fields[0]),
                  pid != getpid(),
                  URL(fileURLWithPath: String(fields[1])).lastPathComponent == "plug",
                  fields.dropFirst(2).contains("connect")
            else { return nil }
            return pid
        }
    }

    private struct Inspection {
        let snapshot: DaemonServiceSnapshot
        let handshake: OperatorHandshake?
    }

    private func inspectWithHandshake(
        canonical: VerifiedAppInstallation,
        legacyPaths: Set<URL>
    ) async throws -> Inspection {
        let ownership = try await launchdInspector.daemonJobs(
            canonical: canonical,
            recognizedLegacyPaths: legacyPaths
        )
        let handshake: OperatorHandshake?
        switch ownership {
        case .appManagedCurrent, .appManagedStale, .recognizedLegacy:
            handshake = try? await backend.handshake()
        case .unmanaged, .unknown:
            handshake = nil
        }
        return Inspection(
            snapshot: DaemonServiceSnapshot(
                ownership: ownership,
                daemonVersion: handshake?.daemonVersion,
                daemonExecutable: handshake?.daemonExecutable
            ),
            handshake: handshake
        )
    }

    private func replace(
        verifiedRecords: [LaunchdJobRecord],
        canonical: VerifiedAppInstallation,
        expectedVersion: String,
        replacingAppRegistration: Bool,
        inspectionPaths: Set<URL>,
        bootOutLoadedJobs: Bool
    ) async throws -> OperatorHandshake {
        guard verifiedRecords.allSatisfy({ $0.programURL != nil }) else {
            throw DaemonServiceError.invalidJobEvidence
        }

        let paused = await backend.pauseConnectors()
        defer { backend.resumeConnectors(paused) }

        // SMAppService owns app-managed jobs. Unregister it while the service
        // still knows the old registration; booting the launchd job out first
        // can make `status` flip while leaving Background Task Management tied
        // to the previous bundle build, which then fails every new spawn.
        if replacingAppRegistration {
            try await backend.unregisterAgent()
        } else {
            if bootOutLoadedJobs {
                for record in verifiedRecords {
                    try await backend.bootOut(record)
                }
            }
            if backend.enabled { try await backend.unregisterAgent() }
        }
        if !backend.enabled {
            do {
                try backend.registerAgent()
            } catch {
                // The old registration is already gone, so a failure here
                // leaves no daemon at all. The system is sometimes still
                // settling from the unregister; a second try a moment later
                // gets past that.
                await backend.waitBeforeRetry()
                try backend.registerAgent()
            }
        }
        guard backend.enabled else { throw DaemonServiceError.registrationDisabled }

        // Starting every configured upstream can take tens of seconds. A
        // force-kickstart inside this verification loop kills that healthy
        // cold start before it can bind the IPC socket, creating a permanent
        // restart cycle. Start exactly once, then give the same process a
        // bounded window to prove its version and executable.
        try await backend.kickstartAgent()
        let (proven, actualVersion) = try await awaitProof(
            canonical: canonical,
            expectedVersion: expectedVersion,
            inspectionPaths: inspectionPaths
        )
        if let proven { return proven }
        throw DaemonServiceError.verificationFailed(
            expectedVersion: expectedVersion,
            actualVersion: actualVersion
        )
    }

    /// Gives the running daemon the startup budget the CLI gives it to prove
    /// its version and executable. Returns the last version it reported when
    /// it never does.
    private func awaitProof(
        canonical: VerifiedAppInstallation,
        expectedVersion: String,
        inspectionPaths: Set<URL>
    ) async throws -> (OperatorHandshake?, String?) {
        var actualVersion: String?
        let deadline = backend.now + startupBudget
        repeat {
            await backend.waitBeforeRetry()
            let inspection = try await inspectWithHandshake(
                canonical: canonical,
                legacyPaths: inspectionPaths
            )
            actualVersion = inspection.snapshot.daemonVersion
            if case let .appManagedCurrent(record) = inspection.snapshot.ownership,
               let handshake = inspection.handshake,
               canonical.isOwnCurrentDaemon(record, version: handshake.daemonVersion, executable: handshake.daemonExecutable)
            {
                return (handshake, actualVersion)
            }
        } while backend.now < deadline
        return (nil, actualVersion)
    }

    private func replaceAppOwned(
        snapshot: DaemonServiceSnapshot,
        record: LaunchdJobRecord,
        canonical: VerifiedAppInstallation,
        expectedVersion: String
    ) async throws -> OperatorHandshake {
        let current = try await launchdInspector.daemonJobs(
            canonical: canonical,
            recognizedLegacyPaths: legacyPaths
        )
        guard current == snapshot.ownership else { throw DaemonServiceError.evidenceChanged }
        return try await replace(
            verifiedRecords: [record],
            canonical: canonical,
            expectedVersion: expectedVersion,
            replacingAppRegistration: true,
            inspectionPaths: inspectionPaths(including: snapshot),
            bootOutLoadedJobs: true
        )
    }

    private func inspectionPaths(including snapshot: DaemonServiceSnapshot) -> Set<URL> {
        legacyPaths.union(snapshot.ownership.programURLs)
    }

    private func verifiedApp(expectedVersion: String) async throws -> VerifiedAppInstallation {
        let canonical = try await appInspector.inspectCurrentApp()
        guard canonical.appVersion == expectedVersion else {
            throw DaemonServiceError.invalidAppVersion(
                expected: expectedVersion,
                actual: canonical.appVersion
            )
        }
        return canonical
    }

    /// A launchd job that runs one of these, after symlinks, is a leftover
    /// install. A link that resolves into the app's own binary still counts.
    private static var defaultLegacyPaths: Set<URL> {
        LegacyPlugProgram.installPaths(home: FileManager.default.homeDirectoryForCurrentUser)
    }
}

@MainActor
private final class SystemDaemonServiceBackend: DaemonServiceBackend {
    private let agent = SMAppService.agent(plistName: DaemonServiceManager.agentPlistName)
    private let runner: any ProcessRunning
    private let probe: LaunchdJobProbe

    init(runner: any ProcessRunning = ProcessRunner()) {
        self.runner = runner
        probe = LaunchdJobProbe(runner: runner)
    }

    var enabled: Bool { agent.status == .enabled }

    func pauseConnectors() async -> [Int32] {
        DaemonServiceManager.connectorPIDs(psOutput: await currentUserProcessList()).compactMap { pid in
            kill(pid, SIGSTOP) == 0 ? pid : nil
        }
    }

    func resumeConnectors(_ pids: [Int32]) {
        for pid in pids { _ = kill(pid, SIGCONT) }
    }

    func resumeStrandedConnectors() async {
        resumeConnectors(DaemonServiceManager.connectorPIDs(psOutput: await currentUserProcessList()))
    }

    func bootOut(_ record: LaunchdJobRecord) async throws {
        guard record.programURL != nil else { throw DaemonServiceError.invalidJobEvidence }
        switch try await probe.verify(record) {
        case .unchanged:
            try await launchctl(["bootout", "gui/\(getuid())/\(record.label)"])
        case .vanished:
            // Already torn down, by a formula uninstall or an earlier bootout.
            // The end state this call wanted is the state on the machine.
            return
        case .replaced:
            throw DaemonServiceError.evidenceChanged
        }
    }

    func unregisterAgent() async throws {
        try await withCheckedThrowingContinuation {
            (continuation: CheckedContinuation<Void, Error>) in
            // ServiceManagement delivers this reply on an XPC background queue,
            // never on the main queue. The closure is written inside a
            // @MainActor type and `unregister(completionHandler:)` imports its
            // handler as non-Sendable, so without the explicit `@Sendable` the
            // compiler treats the body as MainActor-isolated and emits an
            // executor check that traps the moment the reply arrives. Marking
            // it `@Sendable` makes the body nonisolated, which is what the
            // callback contract actually is.
            agent.unregister { @Sendable error in
                if let error {
                    continuation.resume(throwing: error)
                } else {
                    continuation.resume()
                }
            }
        }
        for _ in 0..<60 where agent.status == .enabled {
            try? await Task.sleep(for: .milliseconds(50))
        }
    }

    func registerAgent() throws {
        try agent.register()
    }

    func kickstartAgent() async throws {
        // No `-k`: registration already started the job through RunAtLoad,
        // and killing that fresh instance runs into launchd's ten-second
        // respawn throttle, leaving no daemon for about eleven seconds. A plain
        // kickstart starts the job only when it is not already running.
        try await launchctl(["kickstart", "gui/\(getuid())/\(DaemonServiceManager.daemonLabel)"])
    }

    func handshake() async throws -> OperatorHandshake {
        // Called several times a reconcile and every retry while a
        // replacement proves itself, so each probe closes its own socket.
        try await PlugIPCClient(clientVersion: AppModel.defaultClientVersion).handshakeAndDisconnect()
    }

    var now: ContinuousClock.Instant { .now }

    func waitBeforeRetry() async {
        try? await Task.sleep(for: DaemonServiceManager.retryInterval)
    }

    func openLoginItemSettings() {
        SMAppService.openSystemSettingsLoginItems()
    }

    private func launchctl(_ arguments: [String]) async throws {
        let result = try await runner.run(
            executable: URL(fileURLWithPath: "/bin/launchctl"),
            arguments: arguments,
            timeout: .seconds(10)
        )
        guard result.status == 0 else {
            let detail = result.stderrText
            throw DaemonServiceError.commandFailed(detail)
        }
    }

    /// Empty on failure. The caller pauses connectors as a courtesy before
    /// replacing the daemon, so an unreadable process list costs a rougher
    /// reconnect, never correctness.
    private func currentUserProcessList() async -> String {
        let result = try? await runner.run(
            executable: URL(fileURLWithPath: "/bin/ps"),
            arguments: ["-x", "-o", "pid=", "-o", "command="],
            timeout: .seconds(5)
        )
        guard let result, result.status == 0 else { return "" }
        return String(decoding: result.stdout, as: UTF8.self)
    }
}

extension DaemonServiceBackend {
    func resumeStrandedConnectors() async {}
}
