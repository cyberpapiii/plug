import Foundation
import Darwin
import PlugIPC
import Security
import ServiceManagement
import XCTest
@testable import Plug

@MainActor
final class UnifiedReconciliationFixtureTests: XCTestCase {
    func testSignedLegacyFixtureAdoptsOnceAndConvergesExactly() async throws {
        let fixture = try SignedReconciliationFixture(mode: .legacy)
        let harness = try makeHarness(fixture: fixture)
        registerTeardown(for: fixture, harness: harness)

        await harness.coordinator.reconcile(trigger: .applicationLaunch)

        guard case let .adoptionRequired(beforeAdoption) = harness.coordinator.state else {
            return XCTFail("Legacy daemon must require one explicit adoption, got \(harness.coordinator.state)")
        }
        XCTAssertEqual(beforeAdoption.service.daemonVersion, fixture.legacyVersion)
        XCTAssertEqual(beforeAdoption.service.ownership, .recognizedLegacy([fixture.legacyJob]))
        XCTAssertTrue(FileManager.default.fileExists(atPath: fixture.cargoURL.path))
        // Uninstalling the formula takes over a running system, so it waits
        // for adoption, and the shell link with it.
        XCTAssertTrue(FileManager.default.fileExists(atPath: fixture.formulaURL.path))
        XCTAssertEqual(try fixture.shellDestination(), fixture.cargoURL.path)
        XCTAssertEqual(
            try String(contentsOf: fixture.clientURL, encoding: .utf8),
            fixture.canonicalClientContents
        )

        await harness.coordinator.reconcile(trigger: .explicitAdoption)

        guard let adopted = healthySnapshot(harness.coordinator.state) else { return }
        assertExactHealthy(adopted, fixture: fixture)
        XCTAssertFalse(FileManager.default.fileExists(atPath: fixture.cargoURL.path))
        XCTAssertFalse(FileManager.default.fileExists(atPath: fixture.formulaURL.path))
        XCTAssertEqual(try fixture.shellDestination(), fixture.canonical.executableURL.path)
        XCTAssertEqual(
            try String(contentsOf: fixture.clientURL, encoding: .utf8),
            fixture.canonicalClientContents
        )
        assertCanonicalClientConfig(at: fixture.clientURL, executable: fixture.canonical.executableURL)
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
        XCTAssertTrue(harness.connector.waitForReconnect())
        XCTAssertEqual(harness.connector.replayCount, 1)
        XCTAssertEqual(harness.connector.sessions, ["stdio-fixture-1", "stdio-fixture-2"])
        XCTAssertTrue(harness.connector.observedReconnect)

        await harness.coordinator.reconcile(trigger: .retry)

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

        let firstRetryState = retried
        let firstRetryArtifacts = fixture.artifactsSnapshot()
        let firstRetryEvents = harness.backend.mutationEvents
        let firstRetryAllCalls = await harness.runner.calls
        let firstRetryRunnerCalls = firstRetryAllCalls.filter { call in
            call.arguments == ["uninstall", "cyberpapiii/tap/plug"]
                || call.arguments == ["repair", "--all", "--output", "json"]
        }
        let firstRetryConnector = harness.connector.snapshot

        await harness.coordinator.reconcile(trigger: .retry)

        guard let retriedAgain = healthySnapshot(harness.coordinator.state) else { return }
        XCTAssertEqual(retriedAgain, firstRetryState)
        XCTAssertEqual(fixture.artifactsSnapshot(), firstRetryArtifacts)
        XCTAssertEqual(harness.backend.mutationEvents, firstRetryEvents)
        let secondRetryAllCalls = await harness.runner.calls
        let secondRetryRunnerCalls = secondRetryAllCalls.filter { call in
            call.arguments == ["uninstall", "cyberpapiii/tap/plug"]
                || call.arguments == ["repair", "--all", "--output", "json"]
        }
        XCTAssertEqual(secondRetryRunnerCalls, firstRetryRunnerCalls)
        XCTAssertEqual(harness.connector.snapshot, firstRetryConnector)
    }

    func testSignedFreshFixtureRepairsCommandWithoutAdoption() async throws {
        let fixture = try SignedReconciliationFixture(mode: .fresh)
        let harness = try makeHarness(fixture: fixture)
        registerTeardown(for: fixture, harness: harness)

        await harness.coordinator.reconcile(trigger: .applicationLaunch)

        guard let snapshot = healthySnapshot(harness.coordinator.state) else { return }
        assertExactHealthy(snapshot, fixture: fixture)
        XCTAssertFalse(FileManager.default.fileExists(atPath: fixture.cargoURL.path))
        XCTAssertFalse(FileManager.default.fileExists(atPath: fixture.formulaURL.path))
        XCTAssertEqual(try fixture.shellDestination(), fixture.canonical.executableURL.path)
        assertCanonicalClientConfig(at: fixture.clientURL, executable: fixture.canonical.executableURL)
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
        let harness = try makeHarness(fixture: fixture, failCargoRemovalOnce: true)
        registerTeardown(for: fixture, harness: harness)

        await harness.coordinator.reconcile(trigger: .applicationLaunch)
        guard case .adoptionRequired = harness.coordinator.state else {
            return XCTFail("Legacy fixture must pause at explicit adoption, got \(harness.coordinator.state)")
        }

        await harness.coordinator.reconcile(trigger: .explicitAdoption)

        guard case .blocked = harness.coordinator.state else {
            return XCTFail("Interrupted cleanup must remain retryable")
        }
        XCTAssertTrue(FileManager.default.fileExists(atPath: fixture.cargoURL.path))
        XCTAssertFalse(FileManager.default.fileExists(atPath: fixture.formulaURL.path))
        XCTAssertEqual(try fixture.shellDestination(), fixture.canonical.executableURL.path)
        XCTAssertTrue(harness.connector.waitForReconnect())
        XCTAssertEqual(harness.connector.replayCount, 1)
        assertProtectedFilesUnchanged(fixture)

        await harness.coordinator.reconcile(trigger: .retry)

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

    func testConnectorShutdownReapsProcessAndSocketAcrossRepeatedRuns() async throws {
        for _ in 0..<3 {
            let fixture = try SignedReconciliationFixture(mode: .legacy)
            do {
                let harness = try makeHarness(fixture: fixture)
                let processID = try XCTUnwrap(harness.connector.processIdentifier)
                XCTAssertTrue(FileManager.default.fileExists(atPath: fixture.ipcSocketURL.path))

                await harness.connector.shutdown()

                let result = Darwin.kill(processID, 0)
                let errorCode = errno
                XCTAssertEqual(result, -1)
                XCTAssertEqual(errorCode, ESRCH)
                XCTAssertFalse(FileManager.default.fileExists(atPath: fixture.ipcSocketURL.path))
            } catch {
                try? FileManager.default.removeItem(at: fixture.rootURL)
                throw error
            }
            try? FileManager.default.removeItem(at: fixture.rootURL)
        }
    }

    func testHarnessSetupFailureRemovesFixtureRoot() throws {
        let fixture = try SignedReconciliationFixture(mode: .legacy)
        try FileManager.default.createDirectory(
            at: fixture.ipcSocketURL,
            withIntermediateDirectories: true
        )
        defer { try? FileManager.default.removeItem(at: fixture.ipcSocketURL) }
        XCTAssertTrue(FileManager.default.fileExists(atPath: fixture.rootURL.path))

        XCTAssertThrowsError(try makeHarness(fixture: fixture))

        XCTAssertFalse(FileManager.default.fileExists(atPath: fixture.rootURL.path))
    }

    private func makeHarness(
        fixture: SignedReconciliationFixture,
        failCargoRemovalOnce: Bool = false
    ) throws -> ReconciliationHarness {
        do {
            return try makeHarnessWithoutCleanup(
                fixture: fixture,
                failCargoRemovalOnce: failCargoRemovalOnce
            )
        } catch {
            try? FileManager.default.removeItem(at: fixture.rootURL)
            throw error
        }
    }

    private func makeHarnessWithoutCleanup(
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
                runner: runner,
                identityReader: { url in
                    guard url.standardizedFileURL == fixture.cargoURL.standardizedFileURL,
                          (try? String(contentsOf: url, encoding: .utf8)) == "legacy plug \(fixture.legacyVersion)\n"
                    else { return nil }
                    return LegacyBinaryIdentity(
                        sha256: "fixture-cargo"
                    )
                }
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
        let connector = try FixtureConnectorReplay(
            executableURL: fixture.canonical.executableURL,
            homeURL: fixture.homeURL,
            socketURL: fixture.ipcSocketURL,
            enabled: fixture.mode == .legacy,
            daemonVersion: fixture.version
        )
        let backend = FixtureDaemonBackend(
            enabled: fixture.mode == .fresh,
            legacyVersion: fixture.legacyVersion,
            currentVersion: fixture.version,
            legacyExecutable: fixture.cargoURL,
            currentExecutable: fixture.canonical.executableURL,
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
            serviceDisabled: ServiceDisabledMarker(home: makeTestHome()),
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

    private func registerTeardown(
        for fixture: SignedReconciliationFixture,
        harness: ReconciliationHarness
    ) {
        let connector = harness.connector
        let rootURL = fixture.rootURL
        addTeardownBlock {
            await connector.shutdown()
            try? FileManager.default.removeItem(at: rootURL)
        }
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

    private func assertCanonicalClientConfig(at url: URL, executable: URL) {
        guard let data = try? Data(contentsOf: url),
              let root = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let servers = root["mcpServers"] as? [String: Any],
              let plug = servers["plug"] as? [String: Any]
        else {
            return XCTFail("client repair must write JSON mcpServers.plug entry")
        }
        XCTAssertEqual(plug["command"] as? String, executable.path)
        XCTAssertEqual(plug["args"] as? [String], ["connect"])
        XCTAssertFalse(String(decoding: data, as: UTF8.self).contains("CANONICAL"))
    }
}

private struct ReconciliationHarness {
    let coordinator: InstallationCoordinator
    let runner: FixtureProcessRunner
    let launchd: FixtureLaunchdTimeline
    let backend: FixtureDaemonBackend
    let connector: FixtureConnectorReplay
}

private struct FixtureArtifacts: Equatable {
    let shellDestination: String?
    let clientContents: String?
    let unknownClientContents: String?
    let cargoExists: Bool
    let formulaExists: Bool
    let configContents: String?
    let credentialContents: String?
    let unknownDecoyContents: String?
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
    let ipcSocketURL: URL
    let configURL: URL
    let credentialURL: URL
    let unknownDecoyURL: URL
    let logURL: URL
    var canonicalClientContents: String {
        let object: [String: Any] = [
            "mcpServers": [
                "plug": [
                    "command": canonical.executableURL.path,
                    "args": ["connect"],
                ],
            ],
        ]
        let data = try! JSONSerialization.data(withJSONObject: object, options: [.sortedKeys])
        return String(decoding: data, as: UTF8.self)
    }
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
            embeddedVersion: appVersion
        )
        formulaURL = rootURL.appending(path: "homebrew/opt/plug/bin/plug")
        brewURL = rootURL.appending(path: "homebrew/bin/brew")
        cargoURL = homeURL.appending(path: ".cargo/bin/plug")
        shellURL = homeURL.appending(path: ".local/bin/plug")
        clientURL = rootURL.appending(path: "Library/Application Support/Claude/claude_desktop_config.json")
        unknownClientURL = rootURL.appending(path: "Library/Application Support/Other/plug.json")
        ipcSocketURL = URL(
            fileURLWithPath: "/tmp/plug-app-task7-\(UUID().uuidString.prefix(8)).sock"
        )
        configURL = rootURL.appending(path: "Library/Application Support/Plug/config.toml")
        credentialURL = rootURL.appending(path: "Library/Keychains/plug-credential")
        unknownDecoyURL = rootURL.appending(path: "decoys/plug-not-owned")
        logURL = rootURL.appending(path: "reconciliation.log")
        legacyJob = LaunchdJobRecord(
            label: "local.fixture.plug",
            programURL: cargoURL,
            parentBundleIdentifier: nil,
            parentBundleVersion: nil
        )
        currentJob = LaunchdJobRecord(
            label: "com.plug.daemon",
            programURL: executableURL,
            parentBundleIdentifier: AppInstallationInspector.bundleIdentifier,
            parentBundleVersion: appBuild
        )
        launchdDecoyJob = LaunchdJobRecord(
            label: "com.example.unrelated",
            programURL: rootURL.appending(path: "decoys/other-daemon"),
            parentBundleIdentifier: "com.example.other",
            parentBundleVersion: "1"
        )

        try createBundle(from: signedHost)
        try createProtectedFixtures()
        if mode == .legacy { try createLegacyFixtures() }
    }

    /// The app's own readers, on the fixture bundle. Only the version run
    /// differs: PLUG_DEV keeps the copied binary from handing `--version` to
    /// an installed Plug.app.
    var inspector: AppInstallationInspector {
        let bundleURL = self.bundleURL
        return AppInstallationInspector(
            bundleURL: { bundleURL },
            signatureReader: AppInstallationInspector.readSignature,
            infoReader: AppInstallationInspector.readBundleInfo,
            embeddedVersionReader: {
                try await AppInstallationInspector.readEmbeddedVersion(at: $0, runner: DevModeRunner())
            }
        )
    }

    func shellDestination() throws -> String {
        try FileManager.default.destinationOfSymbolicLink(atPath: shellURL.path)
    }

    func artifactsSnapshot() -> FixtureArtifacts {
        FixtureArtifacts(
            shellDestination: try? shellDestination(),
            clientContents: try? String(contentsOf: clientURL, encoding: .utf8),
            unknownClientContents: try? String(contentsOf: unknownClientURL, encoding: .utf8),
            cargoExists: FileManager.default.fileExists(atPath: cargoURL.path),
            formulaExists: FileManager.default.fileExists(atPath: formulaURL.path),
            configContents: try? String(contentsOf: configURL, encoding: .utf8),
            credentialContents: try? String(contentsOf: credentialURL, encoding: .utf8),
            unknownDecoyContents: try? String(contentsOf: unknownDecoyURL, encoding: .utf8)
        )
    }

    private func createBundle(from signedHost: URL) throws {
        try FileManager.default.createDirectory(
            at: bundleURL.deletingLastPathComponent(),
            withIntermediateDirectories: true
        )
        // Copy the signed host bundle into an isolated temporary app. The
        // embedded executable then has a real temporary path while retaining
        // its signed code and resources.
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
        var candidates: [URL] = []
        if let products = ProcessInfo.processInfo.environment["BUILT_PRODUCTS_DIR"] {
            candidates.append(URL(fileURLWithPath: products).appending(path: "Plug.app"))
        }
        candidates.append(Bundle.main.bundleURL)
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

/// Runs a command with `PLUG_DEV=1` in its environment.
private struct DevModeRunner: ProcessRunning {
    func run(executable: URL, arguments: [String], timeout: Duration) async throws -> ProcessResult {
        try await ProcessRunner().run(
            executable: URL(fileURLWithPath: "/usr/bin/env"),
            arguments: ["PLUG_DEV=1", executable.path] + arguments,
            timeout: timeout
        )
    }
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

        if executable == canonicalExecutable {
            if arguments == ["repair", "--all", "--dry-run", "--output", "json"] {
                return try .golden(clientNeedsRepair ? "repair_needed.json" : "repair_clean.json")
            }
            if arguments == ["repair", "--all", "--output", "json"] {
                if clientNeedsRepair {
                    try Data(canonicalClientContents.utf8).write(to: clientURL, options: .atomic)
                    clientNeedsRepair = false
                }
                return try .golden("repair_clean.json")
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
            cargoBinaryIdentity: observed.cargoBinaryIdentity,
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
        proof: OwnDaemonProof
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
    struct Snapshot: Equatable, Sendable {
        let sessions: [String]
        let replayCount: Int
        let observedReconnect: Bool
        let requestTypes: [String]
    }

    private let lock = NSLock()
    private let outputCondition = NSCondition()
    private let workerGroup = DispatchGroup()
    private let process: Process?
    private let executableURL: URL?
    private let daemonVersion: String?
    private let input: FileHandle?
    private let outputWrite: FileHandle?
    private let errorWrite: FileHandle?
    private let outputRead: FileHandle?
    private let errorRead: FileHandle?
    private let outputDescriptor: Int32?
    private let errorDescriptor: Int32?
    private let listener: Int32?
    private let socketURL: URL?
    private var activeConnection: Int32 = -1
    private var connections = Set<Int32>()
    private var stopped = false
    private var socketOwned = false
    private var shutdownStarted = false
    private var shutdownComplete = false
    private let shutdownFinished = DispatchSemaphore(value: 0)
    private var sessionStorage: [String] = []
    private var replayStorage = 0
    private var registerStorage = 0
    private var requestStorage: [String] = []
    private var outputBuffer = Data()
    private var outputLines: [String] = []
    private var stderrBuffer = Data()
    private var nextRequestID = 1

    init(
        executableURL: URL,
        homeURL: URL,
        socketURL: URL,
        enabled: Bool,
        daemonVersion: String
    ) throws {
        guard enabled else {
            process = nil
            self.executableURL = nil
            self.daemonVersion = nil
            input = nil
            outputWrite = nil
            errorWrite = nil
            outputRead = nil
            errorRead = nil
            outputDescriptor = nil
            errorDescriptor = nil
            listener = nil
            self.socketURL = nil
            return
        }

        self.socketURL = socketURL.standardizedFileURL
        self.executableURL = executableURL.standardizedFileURL
        self.daemonVersion = daemonVersion
        let listener = try TestSocket.listen(at: socketURL.path, backlog: 4)
        self.listener = listener
        socketOwned = true

        let inputPipe = Pipe()
        let outputPipe = Pipe()
        let errorPipe = Pipe()
        let process = Process()
        process.executableURL = executableURL
        process.arguments = ["connect"]
        var environment = ProcessInfo.processInfo.environment
        environment["PLUG_DEV"] = "1"
        environment["PLUG_SOCKET_PATH"] = socketURL.path
        environment["HOME"] = homeURL.path
        environment["PLUG_LOG"] = "error"
        process.environment = environment
        process.standardInput = inputPipe
        process.standardOutput = outputPipe
        process.standardError = errorPipe
        self.process = process
        self.input = inputPipe.fileHandleForWriting
        self.outputWrite = outputPipe.fileHandleForWriting
        self.errorWrite = errorPipe.fileHandleForWriting
        self.outputRead = outputPipe.fileHandleForReading
        self.errorRead = errorPipe.fileHandleForReading
        self.outputDescriptor = outputPipe.fileHandleForReading.fileDescriptor
        self.errorDescriptor = errorPipe.fileHandleForReading.fileDescriptor

        do {
            try process.run()
        } catch {
            shutdownSynchronously()
            throw error
        }
        Self.startOutputReader(
            outputDescriptor,
            owner: self
        )
        Self.startOutputReader(
            errorDescriptor,
            owner: self,
            stderr: true
        )
        Self.startDaemonLoop(listener: listener, owner: self)

        sendMCP(Self.initializeMessage(id: 1))
        guard waitForRequest(matching: "Capabilities", count: 2, timeout: 5) else {
            let detail = diagnostic()
            shutdownSynchronously()
            throw FixtureConnectorError.processDidNotInitialize(detail)
        }
        sendMCP(Self.notificationMessage(method: "notifications/initialized"))
        sendMCP(Self.requestMessage(id: 2, method: "tools/list"))
        guard waitForRequest(matching: "tools/list", count: 1, timeout: 5) else {
            let detail = diagnostic()
            shutdownSynchronously()
            throw FixtureConnectorError.processDidNotListTools(detail)
        }
    }

    deinit { shutdownSynchronously() }

    var processIdentifier: Int32? {
        guard let process else { return nil }
        return process.processIdentifier > 0 ? process.processIdentifier : nil
    }

    func shutdown() async {
        await withCheckedContinuation { continuation in
            DispatchQueue.global(qos: .utility).async { [self] in
                shutdownSynchronously()
                continuation.resume()
            }
        }
    }

    var sessions: [String] {
        lock.lock(); defer { lock.unlock() }
        return sessionStorage
    }

    var replayCount: Int {
        lock.lock(); defer { lock.unlock() }
        return replayStorage
    }

    var observedReconnect: Bool {
        lock.lock(); defer { lock.unlock() }
        return registerStorage >= 2 && replayStorage >= 1
    }

    func waitForReconnect(timeout: TimeInterval = 5) -> Bool {
        let deadline = Date(timeIntervalSinceNow: timeout)
        repeat {
            if observedReconnect { return true }
            Thread.sleep(forTimeInterval: 0.01)
        } while Date() < deadline
        return false
    }

    var snapshot: Snapshot {
        lock.lock(); defer { lock.unlock() }
        return Snapshot(
            sessions: sessionStorage,
            replayCount: replayStorage,
            observedReconnect: registerStorage >= 2 && replayStorage >= 1,
            // Heartbeat pings are liveness observations, not reconciliation
            // mutations. Exclude them so a healthy retry compares stable
            // replay/session evidence instead of timing-sensitive watchdog
            // traffic.
            requestTypes: requestStorage.filter { $0 != "Ping" }
        )
    }

    func pause() -> [Int32] {
        lock.lock()
        let connections = self.connections
        self.connections.removeAll()
        activeConnection = -1
        lock.unlock()
        for connection in connections where connection >= 0 {
            _ = Darwin.shutdown(connection, SHUT_RDWR)
            Darwin.close(connection)
        }
        guard let process, process.isRunning else { return [] }
        return [process.processIdentifier]
    }

    func resume(_ pids: [Int32]) {
        guard !pids.isEmpty else { return }
        // A real stdio adapter only reconnects once it has traffic to forward.
        // Drive one safe request after replacement, then wait for the actual
        // Register/Capabilities/replay frames from the rebuilt daemon session.
        let group = workerGroup
        group.enter()
        DispatchQueue.global(qos: .utility).async { [weak self] in
            defer { group.leave() }
            guard let self else { return }
            Thread.sleep(forTimeInterval: 0.05)
            guard !self.isStopped else { return }
            let expectedRegisterCount = self.registerCount + 1
            let id = self.reserveRequestID()
            self.sendMCP(Self.requestMessage(id: id, method: "tools/list"))
            _ = self.waitForRequest(matching: "Register", count: expectedRegisterCount, timeout: 5)
            _ = self.waitForRequest(matching: "tools/list", count: 2, timeout: 5)
        }
    }

    private var registerCount: Int {
        lock.lock(); defer { lock.unlock() }
        return registerStorage
    }

    private func reserveRequestID() -> Int {
        lock.lock(); defer { lock.unlock() }
        defer { nextRequestID += 1 }
        return nextRequestID
    }

    private func sendMCP(_ message: Data) {
        lock.lock()
        guard !stopped, let input else {
            lock.unlock()
            return
        }
        lock.unlock()
        try? input.write(contentsOf: message)
    }

    private func waitForRequest(
        matching needle: String,
        count: Int,
        timeout: TimeInterval
    ) -> Bool {
        let deadline = Date(timeIntervalSinceNow: timeout)
        repeat {
            if isStopped { return false }
            lock.lock()
            let observed = requestStorage.filter { $0.contains(needle) }.count
            lock.unlock()
            if observed >= count { return true }
            Thread.sleep(forTimeInterval: 0.01)
        } while Date() < deadline
        return false
    }

    private var isStopped: Bool {
        lock.lock()
        defer { lock.unlock() }
        return stopped
    }

    private func shutdownSynchronously() {
        lock.lock()
        if shutdownComplete {
            lock.unlock()
            return
        }
        if shutdownStarted {
            lock.unlock()
            _ = shutdownFinished.wait(timeout: .now() + 5)
            return
        }
        shutdownStarted = true
        stopped = true
        let connections = self.connections
        self.connections.removeAll()
        activeConnection = -1
        let listener = self.listener
        let socketURL = self.socketURL
        let socketOwned = self.socketOwned
        lock.unlock()

        for connection in connections where connection >= 0 {
            _ = Darwin.shutdown(connection, SHUT_RDWR)
            Darwin.close(connection)
        }
        if let listener, listener >= 0 {
            _ = Darwin.shutdown(listener, SHUT_RDWR)
            Darwin.close(listener)
        }
        terminateProcessBounded()
        outputWrite?.closeFile()
        errorWrite?.closeFile()
        _ = workerGroup.wait(timeout: .now() + 5)
        outputRead?.closeFile()
        errorRead?.closeFile()
        input?.closeFile()
        if socketOwned, let socketURL, Self.isTestOwnedSocket(socketURL) {
            try? FileManager.default.removeItem(at: socketURL)
        }

        lock.lock()
        shutdownComplete = true
        lock.unlock()
        shutdownFinished.signal()
    }

    private func terminateProcessBounded() {
        guard let process else { return }
        guard process.isRunning else {
            process.waitUntilExit()
            return
        }
        process.terminate()
        let deadline = Date(timeIntervalSinceNow: 0.25)
        while process.isRunning, Date() < deadline {
            Thread.sleep(forTimeInterval: 0.01)
        }
        if process.isRunning {
            _ = Darwin.kill(process.processIdentifier, SIGKILL)
        }
        process.waitUntilExit()
    }

    private static func isTestOwnedSocket(_ url: URL) -> Bool {
        let temporaryDirectory = FileManager.default.temporaryDirectory.standardizedFileURL
        let candidate = url.standardizedFileURL
        let name = candidate.lastPathComponent
        let directory = candidate.deletingLastPathComponent()
        return (directory == temporaryDirectory || directory.path == "/tmp")
            && name.hasPrefix("plug-app-task7-")
            && name.hasSuffix(".sock")
    }

    private func diagnostic() -> String {
        lock.lock()
        let requests = requestStorage
        let status = process?.isRunning == true ? "running" : "exited"
        let stderr = String(decoding: stderrBuffer, as: UTF8.self)
        let termination = process.map {
            "status=\($0.terminationStatus), reason=\($0.terminationReason.rawValue)"
        } ?? "none"
        lock.unlock()
        outputCondition.lock()
        let output = outputLines
        outputCondition.unlock()
        return "status=\(status), termination=\(termination), requests=\(requests), output=\(output), stderr=\(stderr)"
    }

    private static func startOutputReader(
        _ descriptor: Int32?,
        owner: FixtureConnectorReplay,
        stderr: Bool = false
    ) {
        guard let descriptor else { return }
        let group = owner.workerGroup
        group.enter()
        DispatchQueue.global(qos: .utility).async { [weak owner] in
            defer { group.leave() }
            while true {
                guard let owner else { break }
                guard let data = Self.readByte(
                    from: descriptor,
                    shouldStop: { owner.isStopped }
                ) else { break }
                if data.isEmpty { break }
                if stderr {
                    owner.lock.lock()
                    owner.stderrBuffer.append(data)
                    owner.lock.unlock()
                    continue
                }
                owner.outputCondition.lock()
                owner.outputBuffer.append(data)
                while let newline = owner.outputBuffer.firstIndex(of: 0x0a) {
                    let line = String(
                        decoding: owner.outputBuffer[..<newline],
                        as: UTF8.self
                    )
                    owner.outputLines.append(line)
                    owner.outputBuffer.removeSubrange(...newline)
                }
                owner.outputCondition.broadcast()
                owner.outputCondition.unlock()
            }
        }
    }

    private static func readByte(
        from descriptor: Int32,
        shouldStop: () -> Bool
    ) -> Data? {
        var byte: UInt8 = 0
        while true {
            if shouldStop() { return nil }
            let count = Darwin.read(descriptor, &byte, 1)
            if count == 1 { return Data([byte]) }
            if count == 0 { return nil }
            if errno == EINTR { continue }
            if errno == EAGAIN || errno == EWOULDBLOCK {
                Thread.sleep(forTimeInterval: 0.005)
                continue
            }
            return nil
        }
    }

    private static func startDaemonLoop(listener: Int32, owner: FixtureConnectorReplay) {
        let group = owner.workerGroup
        group.enter()
        DispatchQueue.global(qos: .utility).async { [weak owner] in
            defer { group.leave() }
            while true {
                guard let owner else { return }
                owner.lock.lock()
                let shouldStop = owner.stopped
                owner.lock.unlock()
                if shouldStop { return }
                let connection = Darwin.accept(listener, nil, nil)
                if connection < 0 {
                    owner.lock.lock()
                    let stopped = owner.stopped
                    owner.lock.unlock()
                    if stopped { return }
                    continue
                }
                owner.lock.lock()
                if owner.stopped {
                    owner.lock.unlock()
                    _ = Darwin.shutdown(connection, SHUT_RDWR)
                    Darwin.close(connection)
                    return
                }
                owner.connections.insert(connection)
                owner.activeConnection = connection
                owner.lock.unlock()
                let handlerGroup = owner.workerGroup
                handlerGroup.enter()
                DispatchQueue.global(qos: .utility).async { [weak owner] in
                    defer { handlerGroup.leave() }
                    guard let owner else {
                        _ = Darwin.shutdown(connection, SHUT_RDWR)
                        Darwin.close(connection)
                        return
                    }
                    owner.handleDaemonConnection(connection)
                }
            }
        }
    }

    private func handleDaemonConnection(_ connection: Int32) {
        var generation = 0
        defer {
            lock.lock()
            let ownsConnection = connections.remove(connection) != nil
            if activeConnection == connection { activeConnection = -1 }
            lock.unlock()
            if ownsConnection {
                _ = Darwin.shutdown(connection, SHUT_RDWR)
                Darwin.close(connection)
            }
        }

        while let object = TestSocket.readRequest(connection), let type = object["type"] as? String {
            // A registered connector tags its requests and matches each
            // reply by the tag, as the daemon echoes it.
            let reply = { (response: [String: Any]) in
                var response = response
                response["ipc_id"] = object["ipc_id"]
                TestSocket.writeJSON(response, to: connection)
            }
            lock.lock()
            let method = object["method"] as? String
            requestStorage.append(method.map { "\(type):\($0)" } ?? type)
            lock.unlock()
            switch type {
            case "OperatorHandshake":
                guard let executableURL, let daemonVersion else { break }
                reply([
                    "type": "OperatorHandshake",
                    "handshake": [
                        "daemon_version": daemonVersion,
                        "daemon_executable": executableURL.path,
                        "ownership": "app_managed",
                    ],
                ])
            case "Register":
                lock.lock()
                registerStorage += 1
                generation = registerStorage
                let session = "stdio-fixture-\(registerStorage)"
                sessionStorage.append(session)
                let clientID = object["client_id"] as? String ?? "fixture-client"
                lock.unlock()
                reply([
                    "type": "Registered",
                    "protocol_version": object["protocol_version"] as? Int ?? 3,
                    "client_id": clientID,
                    "session_id": session,
                    "modern_downstream_enabled": false,
                    "cancellation_capability": "fixture-cancellation",
                    "request_ceiling_ms": 660_000,
                ])
            case "Capabilities":
                reply(["type": "Capabilities", "capabilities": [:]])
            case "UpdateSession", "UpdateCapabilities", "RestoreResourceSubscriptions":
                if type == "UpdateCapabilities" && generation > 1 {
                    lock.lock()
                    replayStorage += 1
                    lock.unlock()
                }
                reply(["type": "Ok"])
            case "Ping":
                reply(["type": "Pong"])
            case "ModernDownstreamGate":
                reply(["type": "ModernDownstreamGate", "enabled": false])
            case "McpRequest", "McpRequestWithContext":
                let method = object["method"] as? String ?? ""
                switch method {
                case "tools/list":
                    reply(["type": "McpResponse", "payload": ["tools": []]])
                default:
                    reply(["type": "McpResponse", "payload": [:]])
                }
            default:
                reply(["type": "Ok"])
            }
        }
    }

    private static func initializeMessage(id: Int) -> Data {
        requestMessage(
            id: id,
            method: "initialize",
            params: [
                "protocolVersion": "2025-11-25",
                "capabilities": [:],
                "clientInfo": ["name": "plug-task7-fixture", "version": "1"],
            ]
        )
    }

    private static func notificationMessage(method: String) -> Data {
        Data((try! JSONSerialization.data(withJSONObject: ["jsonrpc": "2.0", "method": method])) + Data([0x0a]))
    }

    private static func requestMessage(
        id: Int,
        method: String,
        params: [String: Any] = [:]
    ) -> Data {
        let object: [String: Any] = [
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        ]
        return (try! JSONSerialization.data(withJSONObject: object)) + Data([0x0a])
    }
}

private enum FixtureConnectorError: Error {
    case processDidNotInitialize(String)
    case processDidNotListTools(String)
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
    private let legacyVersion: String
    private let currentVersion: String
    private let legacyExecutable: URL
    private let currentExecutable: URL
    private let launchd: FixtureLaunchdTimeline
    private let connector: FixtureConnectorReplay
    private(set) var events: [Event] = []

    var mutationEvents: [Event] {
        events.filter {
            if case .handshake = $0 { return false }
            return true
        }
    }

    init(
        enabled: Bool,
        legacyVersion: String,
        currentVersion: String,
        legacyExecutable: URL,
        currentExecutable: URL,
        launchd: FixtureLaunchdTimeline,
        connector: FixtureConnectorReplay
    ) {
        self.enabled = enabled
        self.legacyVersion = legacyVersion
        self.currentVersion = currentVersion
        self.legacyExecutable = legacyExecutable
        self.currentExecutable = currentExecutable
        self.launchd = launchd
        self.connector = connector
    }

    func pauseConnectors() async -> [Int32] {
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
        return makeTestHandshake(
            version: version,
            executable: enabled ? currentExecutable : legacyExecutable
        )
    }

    func waitBeforeRetry() async {}

    func openLoginItemSettings() {}
}
