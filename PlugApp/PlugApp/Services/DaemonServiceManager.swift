import Darwin
import Foundation
import PlugIPC
import ServiceManagement

enum DaemonServiceError: Error, Equatable {
    case adoptionRequired
    case unknownOwnership
    case evidenceChanged
    case invalidAppVersion(expected: String, actual: String)
    case invalidJobEvidence
    case registrationDisabled
    case verificationFailed(expectedVersion: String, actualVersion: String?)
    case commandFailed(String)
}

@MainActor
protocol DaemonServiceBackend: AnyObject {
    var enabled: Bool { get }
    var serviceStatus: SMAppService.Status { get }
    func pauseConnectors() -> [Int32]
    func resumeConnectors(_ pids: [Int32])
    func bootOut(_ record: LaunchdJobRecord) async throws
    func unregisterAgent() async throws
    func registerAgent() throws
    func kickstartAgent() async throws
    func handshake() async throws -> OperatorHandshake
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
    private let retryLimit: Int

    init(
        appInspector: any AppInstallationInspecting = AppInstallationInspector(),
        launchdInspector: any LaunchdJobInspecting = LaunchdJobInspector(),
        backend: any DaemonServiceBackend = SystemDaemonServiceBackend(),
        legacyPaths: Set<URL> = DaemonServiceManager.defaultLegacyPaths,
        retryLimit: Int = 3
    ) {
        self.appInspector = appInspector
        self.launchdInspector = launchdInspector
        self.backend = backend
        self.legacyPaths = legacyPaths
        self.retryLimit = max(1, retryLimit)
    }

    var status: SMAppService.Status { backend.serviceStatus }

    // Temporary bridge while AppModel moves to the asynchronous installation snapshot.
    // It never authorizes replacement; every mutation below re-inspects evidence.
    var needsAdoption: Bool { !backend.enabled }

    func inspect(
        canonical: VerifiedAppInstallation,
        legacyPaths: Set<URL>
    ) async throws -> DaemonServiceSnapshot {
        try await inspectWithHandshake(canonical: canonical, legacyPaths: legacyPaths).snapshot
    }

    func adoptRecognizedLegacy(
        snapshot: DaemonServiceSnapshot,
        expectedVersion: String
    ) async throws -> OperatorHandshake {
        guard case let .recognizedLegacy(records) = snapshot.ownership, !records.isEmpty else {
            throw DaemonServiceError.adoptionRequired
        }
        let canonical = try await verifiedApp(expectedVersion: expectedVersion)
        let current = try await launchdInspector.daemonJobs(
            canonical: canonical,
            recognizedLegacyPaths: legacyPaths
        )
        guard current == snapshot.ownership else { throw DaemonServiceError.evidenceChanged }
        return try await replace(
            verifiedRecords: records,
            canonical: canonical,
            expectedVersion: expectedVersion
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

    func ensureRunning(expectedVersion: String) async throws -> OperatorHandshake {
        let canonical = try await verifiedApp(expectedVersion: expectedVersion)
        let inspection = try await inspectWithHandshake(
            canonical: canonical,
            legacyPaths: legacyPaths
        )
        switch inspection.snapshot.ownership {
        case let .appManagedCurrent(record):
            if let handshake = inspection.handshake,
               exactProof(
                   record: record,
                   handshake: handshake,
                   canonical: canonical,
                   expectedVersion: expectedVersion
               )
            {
                return handshake
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

    // Compatibility entry point for the existing first-run button. It remains
    // an explicit operator action and delegates to the evidence-backed methods.
    func adopt() async throws {
        let canonical = try await appInspector.inspectCurrentApp()
        let snapshot = try await inspect(canonical: canonical, legacyPaths: legacyPaths)
        switch snapshot.ownership {
        case .recognizedLegacy:
            _ = try await adoptRecognizedLegacy(
                snapshot: snapshot,
                expectedVersion: canonical.appVersion
            )
        case .unmanaged:
            _ = try await replace(
                verifiedRecords: [],
                canonical: canonical,
                expectedVersion: canonical.appVersion
            )
        case .appManagedCurrent, .appManagedStale:
            _ = try await ensureRunning(expectedVersion: canonical.appVersion)
        case .unknown:
            throw DaemonServiceError.unknownOwnership
        }
    }

    // Compatibility entry point for the existing restart button. Replacement
    // still requires a newly verified app-owned record before any bootout.
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
        expectedVersion: String
    ) async throws -> OperatorHandshake {
        guard verifiedRecords.allSatisfy({ $0.programURL != nil }) else {
            throw DaemonServiceError.invalidJobEvidence
        }

        let paused = backend.pauseConnectors()
        defer { backend.resumeConnectors(paused) }

        for record in verifiedRecords {
            try await backend.bootOut(record)
        }
        if backend.enabled { try await backend.unregisterAgent() }
        if !backend.enabled { try backend.registerAgent() }
        guard backend.enabled else { throw DaemonServiceError.registrationDisabled }

        var actualVersion: String?
        for _ in 0..<retryLimit {
            try await backend.kickstartAgent()
            await backend.waitBeforeRetry()
            let inspection = try await inspectWithHandshake(
                canonical: canonical,
                legacyPaths: legacyPaths
            )
            actualVersion = inspection.snapshot.daemonVersion
            if case let .appManagedCurrent(record) = inspection.snapshot.ownership,
               let handshake = inspection.handshake,
               exactProof(
                   record: record,
                   handshake: handshake,
                   canonical: canonical,
                   expectedVersion: expectedVersion
               )
            {
                return handshake
            }
        }
        throw DaemonServiceError.verificationFailed(
            expectedVersion: expectedVersion,
            actualVersion: actualVersion
        )
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
            expectedVersion: expectedVersion
        )
    }

    private func exactProof(
        record: LaunchdJobRecord,
        handshake: OperatorHandshake,
        canonical: VerifiedAppInstallation,
        expectedVersion: String
    ) -> Bool {
        guard let program = record.programURL else { return false }
        return resolved(program) == resolved(canonical.executableURL)
            && record.parentBundleIdentifier == AppInstallationInspector.bundleIdentifier
            && record.parentBundleVersion == canonical.buildVersion
            && handshake.daemonExecutable.map { resolved($0) == resolved(canonical.executableURL) } == true
            && handshake.daemonVersion == expectedVersion
    }

    private func verifiedApp(expectedVersion: String) async throws -> VerifiedAppInstallation {
        let canonical = try await appInspector.inspectCurrentApp()
        guard canonical.appVersion == expectedVersion,
              canonical.embeddedVersion == expectedVersion
        else {
            throw DaemonServiceError.invalidAppVersion(
                expected: expectedVersion,
                actual: canonical.embeddedVersion
            )
        }
        return canonical
    }

    private func resolved(_ url: URL) -> URL {
        url.standardizedFileURL.resolvingSymlinksInPath().standardizedFileURL
    }

    private static var defaultLegacyPaths: Set<URL> {
        let home = FileManager.default.homeDirectoryForCurrentUser
        return [
            home.appending(path: ".local/bin/plug"),
            home.appending(path: ".cargo/bin/plug"),
            URL(fileURLWithPath: "/opt/homebrew/opt/plug/bin/plug"),
            URL(fileURLWithPath: "/usr/local/opt/plug/bin/plug"),
        ]
    }
}

@MainActor
private final class SystemDaemonServiceBackend: DaemonServiceBackend {
    private let agent = SMAppService.agent(plistName: "com.plug.daemon.plist")
    private let runner: any ProcessRunning

    init(runner: any ProcessRunning = ProcessRunner()) {
        self.runner = runner
    }

    var enabled: Bool { agent.status == .enabled }
    var serviceStatus: SMAppService.Status { agent.status }

    func pauseConnectors() -> [Int32] {
        DaemonServiceManager.connectorPIDs(psOutput: currentUserProcessList()).compactMap { pid in
            kill(pid, SIGSTOP) == 0 ? pid : nil
        }
    }

    func resumeConnectors(_ pids: [Int32]) {
        for pid in pids { _ = kill(pid, SIGCONT) }
    }

    func bootOut(_ record: LaunchdJobRecord) async throws {
        guard record.programURL != nil else { throw DaemonServiceError.invalidJobEvidence }
        try await launchctl(["bootout", "gui/\(getuid())/\(record.label)"])
    }

    func unregisterAgent() async throws {
        try await agent.unregister()
        for _ in 0..<60 where agent.status == .enabled {
            try? await Task.sleep(for: .milliseconds(50))
        }
    }

    func registerAgent() throws {
        try agent.register()
    }

    func kickstartAgent() async throws {
        try await launchctl(["kickstart", "-k", "gui/\(getuid())/com.plug.daemon"])
    }

    func handshake() async throws -> OperatorHandshake {
        try await PlugIPCClient().connect()
    }

    func waitBeforeRetry() async {
        try? await Task.sleep(for: .milliseconds(250))
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
            let detail = String(decoding: result.stderr, as: UTF8.self)
                .trimmingCharacters(in: .whitespacesAndNewlines)
            throw DaemonServiceError.commandFailed(detail)
        }
    }

    private func currentUserProcessList() -> String {
        let process = Process()
        let pipe = Pipe()
        process.executableURL = URL(fileURLWithPath: "/bin/ps")
        process.arguments = ["-x", "-o", "pid=", "-o", "command="]
        process.standardOutput = pipe
        process.standardError = FileHandle.nullDevice
        guard (try? process.run()) != nil else { return "" }
        let data = pipe.fileHandleForReading.readDataToEndOfFile()
        process.waitUntilExit()
        return String(decoding: data, as: UTF8.self)
    }
}
