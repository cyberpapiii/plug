import Foundation
import PlugIPC
import Security
import ServiceManagement
import XCTest
@testable import Plug

@MainActor
final class UnifiedReconciliationFixtureTests: XCTestCase {
    func testSignedLegacyFixtureAdoptsOnceAndConvergesExactly() async throws {
        let fixture = try SignedReconciliationFixture(mode: .legacy)
        defer { try? FileManager.default.removeItem(at: fixture.rootURL) }
        let harness = try makeHarness(fixture: fixture)

        await harness.coordinator.reconcile(trigger: .applicationLaunch)

        guard case let .adoptionRequired(beforeAdoption) = harness.coordinator.state else {
            return XCTFail("Legacy daemon must require one explicit adoption")
        }
        XCTAssertEqual(beforeAdoption.service.daemonVersion, fixture.legacyVersion)
        XCTAssertEqual(beforeAdoption.service.ownership, .recognizedLegacy([fixture.legacyJob]))
        XCTAssertTrue(FileManager.default.fileExists(atPath: fixture.cargoURL.path))
        XCTAssertFalse(FileManager.default.fileExists(atPath: fixture.formulaURL.path))
        XCTAssertEqual(try fixture.shellDestination(), fixture.canonical.executableURL.path)
        XCTAssertEqual(
            try String(contentsOf: fixture.clientURL, encoding: .utf8),
            fixture.canonicalClientContents
        )

        await harness.coordinator.adopt()

        guard let adopted = healthySnapshot(harness.coordinator.state) else { return }
        assertExactHealthy(adopted, fixture: fixture)
        XCTAssertFalse(FileManager.default.fileExists(atPath: fixture.cargoURL.path))
        XCTAssertFalse(FileManager.default.fileExists(atPath: fixture.formulaURL.path))
        XCTAssertEqual(try fixture.shellDestination(), fixture.canonical.executableURL.path)
        XCTAssertEqual(
            try String(contentsOf: fixture.clientURL, encoding: .utf8),
            fixture.canonicalClientContents
        )
        XCTAssertEqual(
            try String(contentsOf: fixture.unknownClientURL, encoding: .utf8),
            fixture.unknownClientContents
        )
        assertProtectedFilesUnchanged(fixture)

        let records = await harness.launchd.recordsSnapshot()
        XCTAssertTrue(records.contains(fixture.currentJob))
        XCTAssertTrue(records.contains(fixture.launchdDecoyJob))
        XCTAssertFalse(records.contains(fixture.legacyJob))

        let calls = await harness.runner.calls
        XCTAssertEqual(
            calls.filter { $0.executable == fixture.brewURL && $0.arguments == ["uninstall", "cyberpapiii/tap/plug"] }.count,
            1
        )
        XCTAssertEqual(
            calls.filter { $0.executable == fixture.canonical.executableURL && $0.arguments == ["repair", "--all", "--output", "json"] }.count,
            1
        )
        let handshakeVersions = harness.backend.events.compactMap { event -> String? in
            guard case let .handshake(version) = event else { return nil }
            return version
        }
        XCTAssertTrue(handshakeVersions.contains(fixture.version))
        XCTAssertEqual(adopted.daemonVersion, fixture.version)
        XCTAssertEqual(harness.connector.replayCount, 1)
        XCTAssertEqual(harness.connector.sessions, ["stdio-fixture-1"])

        await harness.coordinator.retry()

        guard let retried = healthySnapshot(harness.coordinator.state) else { return }
        assertExactHealthy(retried, fixture: fixture)
        XCTAssertEqual(
            harness.backend.events.filter { event in
                if case .bootOut = event { return true }
                return false
            }.count,
            1
        )
        XCTAssertEqual(
            harness.backend.events.filter { event in
                if case .kickstart = event { return true }
                return false
            }.count,
            1
        )
        XCTAssertEqual(harness.connector.replayCount, 1)
        XCTAssertFalse(FileManager.default.fileExists(atPath: fixture.cargoURL.path))
        assertProtectedFilesUnchanged(fixture)
    }

    func testSignedFreshFixtureRepairsCommandWithoutAdoption() async throws {
        let fixture = try SignedReconciliationFixture(mode: .fresh)
        defer { try? FileManager.default.removeItem(at: fixture.rootURL) }
        let harness = try makeHarness(fixture: fixture)

        await harness.coordinator.reconcile(trigger: .applicationLaunch)

        guard let snapshot = healthySnapshot(harness.coordinator.state) else { return }
        assertExactHealthy(snapshot, fixture: fixture)
        XCTAssertFalse(FileManager.default.fileExists(atPath: fixture.cargoURL.path))
        XCTAssertFalse(FileManager.default.fileExists(atPath: fixture.formulaURL.path))
        XCTAssertEqual(try fixture.shellDestination(), fixture.canonical.executableURL.path)
        XCTAssertTrue(
            harness.backend.events.allSatisfy { event in
                if case .bootOut = event { return false }
                if case .register = event { return false }
                return true
            }
        )
        XCTAssertEqual(harness.connector.replayCount, 0)
        let records = await harness.launchd.recordsSnapshot()
        XCTAssertTrue(records.contains(fixture.launchdDecoyJob))
        assertProtectedFilesUnchanged(fixture)
    }

    func testInterruptedCargoCleanupRetriesIdempotently() async throws {
        let fixture = try SignedReconciliationFixture(mode: .legacy)
        defer { try? FileManager.default.removeItem(at: fixture.rootURL) }
        let harness = try makeHarness(fixture: fixture, failCargoRemovalOnce: true)

        await harness.coordinator.reconcile(trigger: .applicationLaunch)
        guard case .adoptionRequired = harness.coordinator.state else {
            return XCTFail("Legacy fixture must pause at explicit adoption")
        }

        await harness.coordinator.adopt()

        guard case .blocked = harness.coordinator.state else {
            return XCTFail("Interrupted cleanup must remain retryable")
        }
        XCTAssertTrue(FileManager.default.fileExists(atPath: fixture.cargoURL.path))
        XCTAssertFalse(FileManager.default.fileExists(atPath: fixture.formulaURL.path))
        XCTAssertEqual(try fixture.shellDestination(), fixture.canonical.executableURL.path)
        XCTAssertEqual(harness.connector.replayCount, 1)
        assertProtectedFilesUnchanged(fixture)

        await harness.coordinator.retry()

        guard let snapshot = healthySnapshot(harness.coordinator.state) else { return }
        assertExactHealthy(snapshot, fixture: fixture)
        XCTAssertFalse(FileManager.default.fileExists(atPath: fixture.cargoURL.path))
        XCTAssertEqual(
            harness.backend.events.filter { event in
                if case .bootOut = event { return true }
                return false
            }.count,
            1
        )
        XCTAssertEqual(
            harness.backend.events.filter { event in
                if case .pause = event { return true }
                return false
            }.count,
            1
        )
        XCTAssertEqual(harness.connector.replayCount, 1)
        assertProtectedFilesUnchanged(fixture)
    }

    private func makeHarness(
        fixture: SignedReconciliationFixture,
        failCargoRemovalOnce: Bool = false
    ) throws -> ReconciliationHarness {
        let runner = FixtureProcessRunner(
            brewURL: fixture.brewURL,
            cargoURL: fixture.cargoURL,
            canonicalExecutable: fixture.canonical.executableURL,
            formulaURL: fixture.formulaURL,
            clientURL: fixture.clientURL,
            canonicalClientContents: fixture.canonicalClientContents,
            version: fixture.version,
            clientNeedsRepair: fixture.mode == .legacy
        )
        let interruptions = FixtureInterruptions(failCargoRemovalOnce: failCargoRemovalOnce)
        let migrator = FixtureLegacyMigrator(
            real: LegacyInstallMigrator(
                homeURL: fixture.homeURL,
                brewURLs: [fixture.brewURL],
                runner: runner
            ),
            formulaURL: fixture.formulaURL,
            interruptions: interruptions
        )
        let launchd = FixtureLaunchdTimeline(
            initialRecords: fixture.mode == .legacy
                ? [fixture.legacyJob, fixture.launchdDecoyJob]
                : [fixture.currentJob, fixture.launchdDecoyJob],
            currentJob: fixture.currentJob
        )
        let connector = FixtureConnectorReplay()
        let backend = FixtureDaemonBackend(
            enabled: fixture.mode == .fresh,
            legacyVersion: fixture.legacyVersion,
            currentVersion: fixture.version,
            launchd: launchd,
            connector: connector
        )
        let daemonManager = DaemonServiceManager(
            appInspector: fixture.inspector,
            launchdInspector: launchd,
            backend: backend,
            legacyPaths: [fixture.cargoURL],
            retryLimit: 1
        )
        let coordinator = InstallationCoordinator(
            appInspector: fixture.inspector,
            legacyMigrator: migrator,
            clientRepairer: ClientRepairService(runner: runner),
            daemonManager: daemonManager,
            logURL: fixture.logURL,
            openURL: { _ in }
        )
        return ReconciliationHarness(
            coordinator: coordinator,
            runner: runner,
            launchd: launchd,
            backend: backend,
            connector: connector
        )
    }

    private func healthySnapshot(_ state: InstallationState) -> InstallationSnapshot? {
        guard case let .healthy(snapshot) = state else {
            XCTFail("Expected healthy installation, got \(state)")
            return nil
        }
        return snapshot
    }

    private func assertExactHealthy(
        _ snapshot: InstallationSnapshot,
        fixture: SignedReconciliationFixture
    ) {
        XCTAssertEqual(snapshot.app, fixture.canonical)
        XCTAssertEqual(snapshot.daemonVersion, fixture.version)
        XCTAssertFalse(snapshot.clientRepairNeeded)
        XCTAssertEqual(snapshot.shellLink, .canonical(fixture.canonical.executableURL))
        XCTAssertEqual(snapshot.service.daemonVersion, fixture.version)
        XCTAssertEqual(
            snapshot.service.ownership,
            .appManagedCurrent(fixture.currentJob)
        )
        XCTAssertTrue(snapshot.shadowInstalls.isEmpty)
    }

    private func assertProtectedFilesUnchanged(_ fixture: SignedReconciliationFixture) {
        XCTAssertEqual(
            try? String(contentsOf: fixture.configURL, encoding: .utf8),
            fixture.configContents
        )
        XCTAssertEqual(
            try? String(contentsOf: fixture.credentialURL, encoding: .utf8),
            fixture.credentialContents
        )
        XCTAssertEqual(
            try? String(contentsOf: fixture.unknownDecoyURL, encoding: .utf8),
            fixture.unknownDecoyContents
        )
    }
}

private struct ReconciliationHarness {
    let coordinator: InstallationCoordinator
    let runner: FixtureProcessRunner
    let launchd: FixtureLaunchdTimeline
    let backend: FixtureDaemonBackend
    let connector: FixtureConnectorReplay
}

private enum FixtureMode: Sendable, Equatable {
    case fresh
    case legacy
}

private struct SignedReconciliationFixture {
    let mode: FixtureMode
    let rootURL: URL
    let homeURL: URL
    let bundleURL: URL
    let canonical: VerifiedAppInstallation
    let version: String
    let buildVersion: String
    let legacyVersion = "0.5.0"
    let formulaURL: URL
    let brewURL: URL
    let cargoURL: URL
    let shellURL: URL
    let clientURL: URL
    let unknownClientURL: URL
    let configURL: URL
    let credentialURL: URL
    let unknownDecoyURL: URL
    let logURL: URL
    let canonicalClientContents = "{\"mcpServers\":{\"plug\":{\"command\":\"CANONICAL\"}}}"
    let unknownClientContents = "{\"mcpServers\":{\"unknown\":{\"command\":\"do-not-touch\"}}}"
    let configContents = "[servers]\nplug = \"preserve\"\n"
    let credentialContents = "fixture-credential-do-not-delete\n"
    let unknownDecoyContents = "unrelated-decoy\n"

    let legacyJob: LaunchdJobRecord
    let currentJob: LaunchdJobRecord
    let launchdDecoyJob: LaunchdJobRecord

    init(mode: FixtureMode) throws {
        self.mode = mode
        rootURL = FileManager.default.temporaryDirectory
            .appending(path: "plug-unified-reconciliation-\(UUID().uuidString)")
            .standardizedFileURL
        homeURL = rootURL.appending(path: "home")
        bundleURL = URL(
            fileURLWithPath: rootURL.appending(path: "Applications/Plug.app").path,
            isDirectory: true
        )
        let signedHost = try Self.signedHostBundleURL()
        let hostInfoURL = signedHost.appending(path: "Contents/Info.plist")
        guard let hostInfoData = try? Data(contentsOf: hostInfoURL),
              let hostInfo = try? PropertyListSerialization.propertyList(
                  from: hostInfoData,
                  options: [],
                  format: nil
              ) as? [String: Any],
              let appVersion = hostInfo["CFBundleShortVersionString"] as? String,
              let appBuild = hostInfo["CFBundleVersion"] as? String
        else { throw CocoaError(.fileReadCorruptFile) }
        version = appVersion
        buildVersion = appBuild
        let executableURL = bundleURL.appending(path: "Contents/Resources/plug")
        canonical = VerifiedAppInstallation(
            bundleURL: bundleURL,
            executableURL: executableURL,
            appVersion: appVersion,
            buildVersion: appBuild,
            embeddedVersion: appVersion,
            teamID: AppInstallationInspector.teamID
        )
        formulaURL = rootURL.appending(path: "homebrew/opt/plug/bin/plug")
        brewURL = rootURL.appending(path: "homebrew/bin/brew")
        cargoURL = homeURL.appending(path: ".cargo/bin/plug")
        shellURL = homeURL.appending(path: ".local/bin/plug")
        clientURL = rootURL.appending(path: "Library/Application Support/Claude/claude_desktop_config.json")
        unknownClientURL = rootURL.appending(path: "Library/Application Support/Other/plug.json")
        configURL = rootURL.appending(path: "Library/Application Support/Plug/config.toml")
        credentialURL = rootURL.appending(path: "Library/Keychains/plug-credential")
        unknownDecoyURL = rootURL.appending(path: "decoys/plug-not-owned")
        logURL = rootURL.appending(path: "reconciliation.log")
        legacyJob = LaunchdJobRecord(
            label: "local.fixture.plug",
            programURL: cargoURL,
            parentBundleIdentifier: nil,
            parentBundleVersion: nil,
            loaded: true
        )
        currentJob = LaunchdJobRecord(
            label: "com.plug.daemon",
            programURL: executableURL,
            parentBundleIdentifier: AppInstallationInspector.bundleIdentifier,
            parentBundleVersion: appBuild,
            loaded: true
        )
        launchdDecoyJob = LaunchdJobRecord(
            label: "com.example.unrelated",
            programURL: rootURL.appending(path: "decoys/other-daemon"),
            parentBundleIdentifier: "com.example.other",
            parentBundleVersion: "1",
            loaded: true
        )

        try createBundle(from: signedHost)
        try createProtectedFixtures()
        if mode == .legacy { try createLegacyFixtures() }
    }

    var inspector: AppInstallationInspector {
        let bundleURL = self.bundleURL
        let executableURL = canonical.executableURL
        let expectedVersion = version
        return AppInstallationInspector(
            bundleURL: { bundleURL },
            signatureReader: { url in
                try fixtureSignatureEvidence(at: url)
            },
            infoReader: { url in
                let infoURL = url.appending(path: "Contents/Info.plist")
                guard let data = try? Data(contentsOf: infoURL),
                      let object = try? PropertyListSerialization.propertyList(
                          from: data,
                          options: [],
                          format: nil
                      ) as? [String: Any],
                      let appVersion = object["CFBundleShortVersionString"] as? String,
                      let buildVersion = object["CFBundleVersion"] as? String
                else { throw AppInstallationError.missingMetadata }
                return [
                    "CFBundleShortVersionString": appVersion,
                    "CFBundleVersion": buildVersion,
                ]
            },
            embeddedVersionReader: { url in
                guard url.standardizedFileURL == executableURL,
                      FileManager.default.isExecutableFile(atPath: url.path)
                else { throw AppInstallationError.missingEmbeddedExecutable(url) }
                // PLUG_DEV keeps the copied binary from delegating to any
                // live installation while still exercising its real --version
                // path and proving the embedded version.
                let result = try await ProcessRunner().run(
                    executable: URL(fileURLWithPath: "/usr/bin/env"),
                    arguments: ["PLUG_DEV=1", url.path, "--version"],
                    timeout: .seconds(3)
                )
                let output = String(decoding: result.stdout, as: UTF8.self)
                    .trimmingCharacters(in: .whitespacesAndNewlines)
                guard result.status == 0,
                      let version = output.split(whereSeparator: \.isWhitespace).last,
                      String(version) == expectedVersion
                else {
                    throw AppInstallationError.embeddedVersionFailure(
                        String(decoding: result.stderr, as: UTF8.self)
                    )
                }
                return String(version)
            }
        )
    }

    func shellDestination() throws -> String {
        try FileManager.default.destinationOfSymbolicLink(atPath: shellURL.path)
    }

    private func createBundle(from signedHost: URL) throws {
        try FileManager.default.createDirectory(
            at: bundleURL.deletingLastPathComponent(),
            withIntermediateDirectories: true
        )
        try FileManager.default.copyItem(at: signedHost, to: bundleURL)
    }

    private func createProtectedFixtures() throws {
        try write(configContents, to: configURL)
        try write(credentialContents, to: credentialURL)
        try write(unknownDecoyContents, to: unknownDecoyURL)
        try write("other-daemon\n", to: launchdDecoyJob.programURL!)
        try write(unknownClientContents, to: unknownClientURL)
        try write(mode == .legacy ? "legacy-client\n" : canonicalClientContents, to: clientURL)
        try write("brew fixture\n", to: brewURL)
    }

    private func createLegacyFixtures() throws {
        try write("legacy plug \(legacyVersion)\n", to: cargoURL)
        try write("homebrew keg plug \(legacyVersion)\n", to: formulaURL)
        try FileManager.default.createDirectory(at: shellURL.deletingLastPathComponent(), withIntermediateDirectories: true)
        try FileManager.default.createSymbolicLink(atPath: shellURL.path, withDestinationPath: cargoURL.path)
    }

    private func write(_ contents: String, to url: URL) throws {
        try FileManager.default.createDirectory(
            at: url.deletingLastPathComponent(),
            withIntermediateDirectories: true
        )
        try Data(contents.utf8).write(to: url, options: .atomic)
    }

    private static func signedHostBundleURL() throws -> URL {
        var candidates = [Bundle.main.bundleURL]
        if let products = ProcessInfo.processInfo.environment["BUILT_PRODUCTS_DIR"] {
            candidates.append(URL(fileURLWithPath: products).appending(path: "Plug.app"))
        }
        guard let bundle = candidates
            .map(\.standardizedFileURL)
            .first(where: {
                $0.pathExtension == "app"
                    && FileManager.default.fileExists(
                        atPath: $0.appending(path: "Contents/Resources/plug").path
                    )
            })
        else { throw CocoaError(.fileNoSuchFile) }
        return bundle
    }
}

private func fixtureSignatureEvidence(at bundleURL: URL) throws -> AppSignatureEvidence {
    var staticCode: SecStaticCode?
    guard SecStaticCodeCreateWithPath(bundleURL as CFURL, [], &staticCode) == errSecSuccess,
          let staticCode
    else { throw AppInstallationError.invalidSignature }

    let requirementText = "anchor apple generic and identifier \"\(AppInstallationInspector.bundleIdentifier)\" and certificate leaf[subject.OU] = \"\(AppInstallationInspector.teamID)\""
    var requirement: SecRequirement?
    guard SecRequirementCreateWithString(requirementText as CFString, [], &requirement) == errSecSuccess,
          let requirement
    else { throw AppInstallationError.invalidSignature }

    var signingInformation: CFDictionary?
    guard SecCodeCopySigningInformation(
        staticCode,
        SecCSFlags(rawValue: kSecCSSigningInformation),
        &signingInformation
    ) == errSecSuccess,
    let information = signingInformation as? [String: Any]
    else { throw AppInstallationError.invalidSignature }

    return AppSignatureEvidence(
        valid: SecStaticCodeCheckValidity(staticCode, [], requirement) == errSecSuccess,
        bundleIdentifier: information[kSecCodeInfoIdentifier as String] as? String ?? "",
        teamID: information[kSecCodeInfoTeamIdentifier as String] as? String ?? ""
    )
}

private actor FixtureProcessRunner: ProcessRunning {
    struct Call: Equatable, Sendable {
        let executable: URL
        let arguments: [String]
    }

    let brewURL: URL
    let cargoURL: URL
    let canonicalExecutable: URL
    let formulaURL: URL
    let clientURL: URL
    let canonicalClientContents: String
    let version: String
    private var clientNeedsRepair: Bool
    private(set) var calls: [Call] = []

    init(
        brewURL: URL,
        cargoURL: URL,
        canonicalExecutable: URL,
        formulaURL: URL,
        clientURL: URL,
        canonicalClientContents: String,
        version: String,
        clientNeedsRepair: Bool
    ) {
        self.brewURL = brewURL
        self.cargoURL = cargoURL
        self.canonicalExecutable = canonicalExecutable
        self.formulaURL = formulaURL
        self.clientURL = clientURL
        self.canonicalClientContents = canonicalClientContents
        self.version = version
        self.clientNeedsRepair = clientNeedsRepair
    }

    func run(executable: URL, arguments: [String], timeout: Duration) async throws -> ProcessResult {
        calls.append(Call(executable: executable, arguments: arguments))

        if executable == brewURL {
            if arguments == ["list", "--versions", "cyberpapiii/tap/plug"] {
                return FileManager.default.fileExists(atPath: formulaURL.path)
                    ? ProcessResult(status: 0, stdout: Data("plug 0.6.4\n".utf8), stderr: Data())
                    : ProcessResult(status: 0, stdout: Data(), stderr: Data())
            }
            if arguments == ["uninstall", "cyberpapiii/tap/plug"] {
                try? FileManager.default.removeItem(at: formulaURL)
                return ProcessResult(status: 0, stdout: Data(), stderr: Data())
            }
        }

        if executable == cargoURL, arguments == ["--version"] {
            return FileManager.default.fileExists(atPath: cargoURL.path)
                ? ProcessResult(status: 0, stdout: Data("plug \(version)\n".utf8), stderr: Data())
                : ProcessResult(status: 1, stdout: Data(), stderr: Data())
        }

        if executable == canonicalExecutable {
            if arguments == ["doctor", "--output", "json"] {
                let json = "{\"unified_install\":{\"client_repair_needed\":\(clientNeedsRepair)}}"
                return ProcessResult(
                    status: clientNeedsRepair ? 2 : 0,
                    stdout: Data(json.utf8),
                    stderr: Data()
                )
            }
            if arguments == ["repair", "--all", "--output", "json"] {
                if clientNeedsRepair {
                    try Data(canonicalClientContents.utf8).write(to: clientURL, options: .atomic)
                    clientNeedsRepair = false
                }
                let json = "{\"items\":[{\"changed\":true},{\"changed\":false}]}"
                return ProcessResult(status: 0, stdout: Data(json.utf8), stderr: Data())
            }
        }

        return ProcessResult(
            status: 127,
            stdout: Data(),
            stderr: Data("unexpected fixture process invocation".utf8)
        )
    }
}

private actor FixtureInterruptions {
    private var failCargoRemovalOnce: Bool

    init(failCargoRemovalOnce: Bool) {
        self.failCargoRemovalOnce = failCargoRemovalOnce
    }

    func consumeCargoRemovalFailure() -> Bool {
        guard failCargoRemovalOnce else { return false }
        failCargoRemovalOnce = false
        return true
    }
}

private struct FixtureLegacyMigrator: LegacyInstallMigrating {
    let real: LegacyInstallMigrator
    let formulaURL: URL
    let interruptions: FixtureInterruptions

    func inspect(canonical: VerifiedAppInstallation) async throws -> LegacyInstallSnapshot {
        let observed = try await real.inspect(canonical: canonical)
        var recognized = observed.recognizedPaths
        if observed.formulaInstalled {
            recognized.insert(formulaURL.standardizedFileURL)
        }
        return LegacyInstallSnapshot(
            formulaInstalled: observed.formulaInstalled,
            cargoBinary: observed.cargoBinary,
            shellLink: observed.shellLink,
            recognizedPaths: recognized,
            unknownPaths: observed.unknownPaths
        )
    }

    func removeRecognizedFormula(_ snapshot: LegacyInstallSnapshot) async throws {
        try await real.removeRecognizedFormula(snapshot)
    }

    func repairShellLink(to executable: URL) async throws -> ShellLinkState {
        try await real.repairShellLink(to: executable)
    }

    func removeVerifiedCargoBinary(
        _ snapshot: LegacyInstallSnapshot,
        proof: ReconciliationProof
    ) async throws {
        if await interruptions.consumeCargoRemovalFailure() {
            throw LegacyInstallError.fileOperation("interrupted fixture cleanup")
        }
        try await real.removeVerifiedCargoBinary(snapshot, proof: proof)
    }
}

private actor FixtureLaunchdTimeline: LaunchdJobInspecting {
    private var records: [LaunchdJobRecord]
    private let currentJob: LaunchdJobRecord

    init(initialRecords: [LaunchdJobRecord], currentJob: LaunchdJobRecord) {
        records = initialRecords
        self.currentJob = currentJob
    }

    func daemonJobs(
        canonical: VerifiedAppInstallation,
        recognizedLegacyPaths: Set<URL>
    ) async throws -> DaemonOwnershipState {
        let currentRecords = records
        return try await LaunchdJobInspector(records: { currentRecords }).daemonJobs(
            canonical: canonical,
            recognizedLegacyPaths: recognizedLegacyPaths
        )
    }

    func bootOut(label: String) {
        records.removeAll { $0.label == label }
    }

    func kickstart() {
        records.removeAll { $0.label == currentJob.label }
        records.append(currentJob)
    }

    func recordsSnapshot() -> [LaunchdJobRecord] { records }
}

private final class FixtureConnectorReplay: @unchecked Sendable {
    private let lock = NSLock()
    private var sessionStorage = ["stdio-fixture-1"]
    private var replayStorage = 0
    private var eventStorage: [String] = []

    var sessions: [String] {
        lock.lock(); defer { lock.unlock() }
        return sessionStorage
    }

    var replayCount: Int {
        lock.lock(); defer { lock.unlock() }
        return replayStorage
    }

    var events: [String] {
        lock.lock(); defer { lock.unlock() }
        return eventStorage
    }

    func pause() -> [Int32] {
        lock.lock(); defer { lock.unlock() }
        eventStorage.append("pause")
        return [4101]
    }

    func resume(_ pids: [Int32]) {
        lock.lock(); defer { lock.unlock() }
        eventStorage.append("resume:\(pids.map(String.init).joined(separator: ","))")
        replayStorage += sessionStorage.count
    }
}

@MainActor
private final class FixtureDaemonBackend: DaemonServiceBackend {
    enum Event: Equatable {
        case pause
        case bootOut(String)
        case register
        case kickstart
        case handshake(String)
        case resume([Int32])
    }

    var enabled: Bool
    var serviceStatus: SMAppService.Status { enabled ? .enabled : .notRegistered }
    private let legacyVersion: String
    private let currentVersion: String
    private let launchd: FixtureLaunchdTimeline
    private let connector: FixtureConnectorReplay
    private(set) var events: [Event] = []

    init(
        enabled: Bool,
        legacyVersion: String,
        currentVersion: String,
        launchd: FixtureLaunchdTimeline,
        connector: FixtureConnectorReplay
    ) {
        self.enabled = enabled
        self.legacyVersion = legacyVersion
        self.currentVersion = currentVersion
        self.launchd = launchd
        self.connector = connector
    }

    func pauseConnectors() -> [Int32] {
        events.append(.pause)
        return connector.pause()
    }

    func resumeConnectors(_ pids: [Int32]) {
        events.append(.resume(pids))
        connector.resume(pids)
    }

    func bootOut(_ record: LaunchdJobRecord) async throws {
        guard record.programURL != nil else { throw DaemonServiceError.invalidJobEvidence }
        events.append(.bootOut(record.label))
        await launchd.bootOut(label: record.label)
    }

    func unregisterAgent() async throws {
        enabled = false
    }

    func registerAgent() throws {
        events.append(.register)
        enabled = true
    }

    func kickstartAgent() async throws {
        events.append(.kickstart)
        await launchd.kickstart()
    }

    func handshake() async throws -> OperatorHandshake {
        let version = enabled ? currentVersion : legacyVersion
        events.append(.handshake(version))
        return fixtureHandshake(version: version)
    }

    func waitBeforeRetry() async {}

    func openLoginItemSettings() {}
}

private func fixtureHandshake(version: String) -> OperatorHandshake {
    let data = try! JSONSerialization.data(withJSONObject: [
        "daemonVersion": version,
        "ipcMin": 3,
        "ipcMax": 4,
        "ownership": "app_managed",
        "capabilities": ["stdio-replay"],
    ])
    return try! JSONDecoder().decode(OperatorHandshake.self, from: data)
}
