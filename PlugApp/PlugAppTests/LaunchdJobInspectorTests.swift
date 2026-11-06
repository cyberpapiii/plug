import Foundation
import XCTest
@testable import Plug
import PlugIPC

final class LaunchdJobInspectorTests: XCTestCase {
    private let canonical = makeTestInstallation()

    func testClassifiesCurrentAndStaleAppManagedJobs() async throws {
        let current = record(label: "com.plug.daemon", program: canonical.executableURL, parentVersion: "20")
        let currentState = try await LaunchdJobInspector(records: { [current] }).daemonJobs(canonical: canonical, recognizedLegacyPaths: [])
        XCTAssertEqual(currentState, .appManagedCurrent(current))

        let stale = record(label: "com.plug.daemon", program: canonical.executableURL, parentVersion: "19")
        let staleState = try await LaunchdJobInspector(records: { [stale] }).daemonJobs(canonical: canonical, recognizedLegacyPaths: [])
        XCTAssertEqual(staleState, .appManagedStale(stale))
    }

    func testLegacyPlugProgramMatchesSharedFixture() throws {
        guard let url = Bundle(for: LaunchdJobInspectorTests.self)
            .url(forResource: "legacy_plug_programs", withExtension: "json")
        else {
            return XCTFail("missing bundled leftover-path fixture")
        }
        let fixture = try JSONDecoder().decode(LegacyPlugProgramFixture.self, from: Data(contentsOf: url))

        // The table, not only the cases: a shape added here but not to the
        // fixture never reaches the Rust copy of the same rules.
        XCTAssertEqual(fixture.rules.exact, LegacyPlugProgram.exactPaths)
        XCTAssertEqual(fixture.rules.homeRelative, LegacyPlugProgram.homeRelativePaths)
        XCTAssertEqual(fixture.rules.cellarRoots, LegacyPlugProgram.cellarRoots)

        for path in fixture.recognized {
            XCTAssertTrue(
                LegacyPlugProgram.isRecognized(URL(fileURLWithPath: path)),
                "expected recognized: \(path)"
            )
        }
        let home = FileManager.default.homeDirectoryForCurrentUser.standardizedFileURL.path
        for suffix in fixture.homeRecognizedSuffixes {
            let path = "\(home)/\(suffix)"
            XCTAssertTrue(
                LegacyPlugProgram.isRecognized(URL(fileURLWithPath: path)),
                "expected recognized: \(path)"
            )
        }
        for path in fixture.unrecognized {
            XCTAssertFalse(
                LegacyPlugProgram.isRecognized(URL(fileURLWithPath: path)),
                "expected unrecognized: \(path)"
            )
        }
    }

    func testLeftoverHomebrewCellarJobIsRecognizedAfterFormulaPathsDisappear() async throws {
        let cellar = URL(fileURLWithPath: "/opt/homebrew/Cellar/plug/0.6.3/bin/plug")
        let leftover = record(label: "com.plug.daemon", program: cellar, parentID: nil, parentVersion: nil)

        let state = try await LaunchdJobInspector(records: { [leftover] }).daemonJobs(
            canonical: canonical,
            recognizedLegacyPaths: []
        )
        XCTAssertEqual(state, .recognizedLegacy([leftover]))
    }

    func testLeftoverHomebrewBinJobIsRecognizedWithoutResolvedOptPath() async throws {
        let brewBin = URL(fileURLWithPath: "/opt/homebrew/bin/plug")
        let leftover = record(label: "com.plug.daemon", program: brewBin, parentID: nil, parentVersion: nil)

        let state = try await LaunchdJobInspector(records: { [leftover] }).daemonJobs(
            canonical: canonical,
            recognizedLegacyPaths: []
        )
        XCTAssertEqual(state, .recognizedLegacy([leftover]))
    }

    func testLeftoverLocalBinJobIsRecognizedWithoutRecognizedPaths() async throws {
        let localBin = FileManager.default.homeDirectoryForCurrentUser
            .appending(path: ".local/bin/plug")
        let leftover = record(label: "com.plug.daemon", program: localBin, parentID: nil, parentVersion: nil)

        let state = try await LaunchdJobInspector(records: { [leftover] }).daemonJobs(
            canonical: canonical,
            recognizedLegacyPaths: []
        )
        XCTAssertEqual(state, .recognizedLegacy([leftover]))
    }

    func testLeftoverHomebrewOptPlugJobIsRecognized() async throws {
        let optPlug = URL(fileURLWithPath: "/opt/homebrew/opt/plug/bin/plug")
        let leftover = record(label: "com.plug.daemon", program: optPlug, parentID: nil, parentVersion: nil)

        let state = try await LaunchdJobInspector(records: { [leftover] }).daemonJobs(
            canonical: canonical,
            recognizedLegacyPaths: []
        )
        XCTAssertEqual(state, .recognizedLegacy([leftover]))
    }

    func testAlternateLabelAtKnownPlugPathIsRecognizedLegacy() async throws {
        let legacyURL = URL(fileURLWithPath: "/Users/me/.cargo/bin/plug")
        let legacy = record(label: "local.claude-rc.plug", program: legacyURL, parentID: nil, parentVersion: nil)

        let state = try await LaunchdJobInspector(records: { [legacy] }).daemonJobs(
            canonical: canonical,
            recognizedLegacyPaths: [legacyURL]
        )
        XCTAssertEqual(state, .recognizedLegacy([legacy]))
    }

    func testExactPlugLabelWithUnrelatedProgramIsUnknownAndPreserved() async throws {
        let unrelated = record(
            label: "com.plug.daemon",
            program: URL(fileURLWithPath: "/tmp/not-plug"),
            parentID: nil,
            parentVersion: nil
        )

        let state = try await LaunchdJobInspector(records: { [unrelated] }).daemonJobs(
            canonical: canonical,
            recognizedLegacyPaths: []
        )
        XCTAssertEqual(state, .unknown([unrelated]))
    }

    func testPlugSubstringLabelWithUnrelatedProgramDoesNotClaimDaemonOwnership() async throws {
        let unrelated = record(
            label: "local.claude-rc.plug",
            program: URL(fileURLWithPath: "/Users/me/.local/share/claude/versions/2.1.246"),
            parentID: nil,
            parentVersion: nil
        )

        let state = try await LaunchdJobInspector(records: { [unrelated] }).daemonJobs(
            canonical: canonical,
            recognizedLegacyPaths: []
        )

        XCTAssertEqual(state, .unmanaged)
    }

    func testRunningPlugApplicationJobIsNotMistakenForDaemonOwnership() async throws {
        let applicationJob = record(
            label: "application.com.cyberpapiii.plug.123.456",
            program: canonical.bundleURL.appending(path: "Contents/MacOS/Plug"),
            parentID: AppInstallationInspector.bundleIdentifier,
            parentVersion: canonical.buildVersion
        )

        let state = try await LaunchdJobInspector(records: { [applicationJob] }).daemonJobs(
            canonical: canonical,
            recognizedLegacyPaths: []
        )

        XCTAssertEqual(state, .unmanaged)
    }

    func testBroadEnumerationFindsPlugProgramBehindUnrelatedLabel() async throws {
        let runner = launchctlPrintingOneOwner()
        let inspector = LaunchdJobInspector(runner: runner, userID: 501)

        let state = try await inspector.daemonJobs(
            canonical: canonical,
            recognizedLegacyPaths: [URL(fileURLWithPath: "/Users/me/.local/bin/plug")]
        )

        let legacy = record(
            label: "unexpected.owner",
            program: URL(fileURLWithPath: "/Users/me/.local/bin/plug"),
            parentID: nil,
            parentVersion: nil
        )
        XCTAssertEqual(state, .recognizedLegacy([legacy]))
        let printed = await runner.arguments.filter { $0 != ["print", "gui/501"] }
        XCTAssertEqual(printed, [["print", "gui/501/unexpected.owner"]])
    }

    func testAppleNamespaceLabelsAreNotInspected() {
        let listing = domain("0\t0\tcom.apple.unrelated\n1\t0\tcom.apple.plug.lookalike\n2\t0\tcom.plug.daemon\n3\t0\tunexpected.owner\n")

        XCTAssertEqual(
            LaunchdJobInspector.parseLabels(listing),
            ["com.plug.daemon", "unexpected.owner"]
        )
    }

    // MARK: The samples `plug/src/service.rs` reads too

    private func sample(_ name: String) throws -> String {
        let root = URL(fileURLWithPath: "\(#filePath)")
            .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
        return try String(contentsOf: root.appending(path: "testdata/launchd/\(name)"), encoding: .utf8)
    }

    func testTheDomainSampleListsTheSameLabelsAsTheCLI() throws {
        XCTAssertEqual(
            LaunchdJobInspector.domainLabels(try sample("gui_domain.txt")),
            ["local.claude-rc.plug", "com.plug.daemon", "com.apple.Finder", "homebrew.mxcl.plug"]
        )
    }

    func testTheServiceManagedSampleIsThisAppsDaemon() async throws {
        let output = try sample("app_service_job.txt")
        let installation = makeTestInstallation(build: "12")
        let state = try await LaunchdJobInspector(records: {
            [LaunchdJobInspector.parseRecord(label: PlugPaths.daemonLabel, output: output)]
        }).daemonJobs(canonical: installation, recognizedLegacyPaths: [])
        guard case let .appManagedCurrent(record) = state else {
            return XCTFail("expected the app's own daemon, got \(state)")
        }
        XCTAssertEqual(record.programURL, installation.executableURL)
        XCTAssertEqual(record.managedBy, LaunchdJobInspector.serviceManagement)
    }

    /// Without `managed_by`, nothing says SMAppService registered the job,
    /// so it is not taken for the app's, as `plug doctor` does not.
    func testAJobSMAppServiceDidNotRegisterIsNotTheAppsDaemon() async throws {
        let output = try sample("app_job_not_service_managed.txt")
        let installation = makeTestInstallation(build: "12")
        let state = try await LaunchdJobInspector(records: {
            [LaunchdJobInspector.parseRecord(label: PlugPaths.daemonLabel, output: output)]
        }).daemonJobs(canonical: installation, recognizedLegacyPaths: [])
        assertUnknown(state, programIdentifier: PlugPaths.bundledBinary)
    }

    func testTheLegacySampleIsALeftoverPlug() async throws {
        let record = LaunchdJobInspector.parseRecord(label: "homebrew.mxcl.plug", output: try sample("legacy_job.txt"))
        XCTAssertEqual(record.programURL?.path, "/opt/homebrew/opt/plug/bin/plug")
        XCTAssertEqual(record.arguments, ["/opt/homebrew/opt/plug/bin/plug", "serve", "--daemon"])
        let state = try await LaunchdJobInspector(records: { [record] })
            .daemonJobs(canonical: canonical, recognizedLegacyPaths: [])
        XCTAssertEqual(state, .recognizedLegacy([record]))
    }

    func testLiveSMAppServiceShapeResolvesBundleRelativeDaemon() async throws {
        let runner = launchctlWithAppServiceJob(
            parentBundleIdentifier: AppInstallationInspector.bundleIdentifier,
            parentBundleVersion: canonical.buildVersion,
            arguments: ["Contents/Resources/plug", "serve", "--daemon"]
        )
        let inspector = LaunchdJobInspector(runner: runner, userID: 501)

        let state = try await inspector.daemonJobs(canonical: canonical, recognizedLegacyPaths: [])

        let expected = record(
            label: "com.plug.daemon",
            program: canonical.executableURL,
            parentID: AppInstallationInspector.bundleIdentifier,
            parentVersion: canonical.buildVersion,
            programIdentifier: "Contents/Resources/plug",
            arguments: ["Contents/Resources/plug", "serve", "--daemon"],
            managedBy: LaunchdJobInspector.serviceManagement
        )
        XCTAssertEqual(state, .appManagedCurrent(expected))
    }

    func testLiveSMAppServiceShapeWithOlderBundleVersionIsRecognizedAsStale() async throws {
        let runner = launchctlWithAppServiceJob(
            parentBundleIdentifier: AppInstallationInspector.bundleIdentifier,
            parentBundleVersion: "19",
            arguments: ["Contents/Resources/plug", "serve", "--daemon"]
        )
        let inspector = LaunchdJobInspector(runner: runner, userID: 501)

        let state = try await inspector.daemonJobs(canonical: canonical, recognizedLegacyPaths: [])

        let expected = record(
            label: "com.plug.daemon",
            program: canonical.executableURL,
            parentID: AppInstallationInspector.bundleIdentifier,
            parentVersion: "19",
            programIdentifier: "Contents/Resources/plug",
            arguments: ["Contents/Resources/plug", "serve", "--daemon"],
            managedBy: LaunchdJobInspector.serviceManagement
        )
        XCTAssertEqual(state, .appManagedStale(expected))
    }

    func testLiveSMAppServiceShapeWithMismatchedBundleIdentifierRemainsUnknown() async throws {
        let runner = launchctlWithAppServiceJob(
            parentBundleIdentifier: "com.example.other",
            parentBundleVersion: canonical.buildVersion,
            arguments: ["Contents/Resources/plug", "serve", "--daemon"]
        )
        let inspector = LaunchdJobInspector(runner: runner, userID: 501)

        let state = try await inspector.daemonJobs(canonical: canonical, recognizedLegacyPaths: [])

        assertUnknown(state, programIdentifier: "Contents/Resources/plug")
    }

    func testLiveSMAppServiceShapeWithMismatchedArgumentsRemainsUnknown() async throws {
        let runner = launchctlWithAppServiceJob(
            parentBundleIdentifier: AppInstallationInspector.bundleIdentifier,
            parentBundleVersion: canonical.buildVersion,
            arguments: ["Contents/Resources/plug", "serve"]
        )
        let inspector = LaunchdJobInspector(runner: runner, userID: 501)

        let state = try await inspector.daemonJobs(canonical: canonical, recognizedLegacyPaths: [])

        assertUnknown(state, programIdentifier: "Contents/Resources/plug")
    }

    func testLaunchctlListFailureIsPropagatedInsteadOfReportingUnmanaged() async {
        let inspector = LaunchdJobInspector(
            runner: launchctlFailing(list: true),
            userID: 501
        )

        do {
            _ = try await inspector.daemonJobs(canonical: canonical, recognizedLegacyPaths: [])
            XCTFail("Expected launchctl list failure")
        } catch let error as LaunchdJobInspectionError {
            XCTAssertEqual(error, .listFailed(status: 113, detail: "list unavailable"))
        } catch {
            XCTFail("Unexpected error: \(error)")
        }
    }

    func testAnyLaunchctlPrintFailureIsPropagatedInsteadOfDroppingTheJob() async {
        let inspector = LaunchdJobInspector(
            runner: launchctlFailing(list: false),
            userID: 501
        )

        do {
            _ = try await inspector.daemonJobs(canonical: canonical, recognizedLegacyPaths: [])
            XCTFail("Expected launchctl print failure")
        } catch let error as LaunchdJobInspectionError {
            XCTAssertEqual(
                error,
                .printFailed(label: "com.plug.daemon", status: 113, detail: "print unavailable")
            )
        } catch {
            XCTFail("Unexpected error: \(error)")
        }
    }

    func testSlowLaunchctlPrintForUnrelatedJobIsIgnored() async throws {
        let inspector = LaunchdJobInspector(
            runner: launchctlTimingOut(on: "com.example.other"),
            userID: 501
        )

        let state = try await inspector.daemonJobs(
            canonical: canonical,
            recognizedLegacyPaths: []
        )

        XCTAssertEqual(state, .unmanaged)
    }

    func testSlowLaunchctlPrintForPlugDaemonIsPropagated() async {
        let inspector = LaunchdJobInspector(
            runner: launchctlTimingOut(on: "com.plug.daemon"),
            userID: 501
        )

        do {
            _ = try await inspector.daemonJobs(canonical: canonical, recognizedLegacyPaths: [])
            XCTFail("Expected the daemon label's timeout to propagate")
        } catch ProcessRunnerError.timedOut {
        } catch {
            XCTFail("Unexpected error: \(error)")
        }
    }

    func testVanishedUnrelatedLaunchdJobIsIgnored() async throws {
        let runner = launchctl(listing: "123\t0\tcom.apple.transient\n") { _ in .notFound }
        let inspector = LaunchdJobInspector(runner: runner, userID: 501)

        let state = try await inspector.daemonJobs(
            canonical: canonical,
            recognizedLegacyPaths: []
        )

        XCTAssertEqual(state, .unmanaged)
    }

    func testLaunchdProgramSymlinkResolvesToRecognizedLegacyBinary() async throws {
        let fixture = try LaunchdSymlinkFixture()
        defer { fixture.cleanup() }
        let runner = launchctlRunning( fixture.shellLink)
        let inspector = LaunchdJobInspector(runner: runner, userID: 501)

        let state = try await inspector.daemonJobs(
            canonical: canonical,
            recognizedLegacyPaths: [fixture.legacyBinary]
        )

        let observed = record(
            label: "local.claude-rc.plug",
            program: fixture.shellLink,
            parentID: nil,
            parentVersion: nil
        )
        XCTAssertEqual(state, .recognizedLegacy([observed]))
    }

    private func record(
        label: String,
        program: URL?,
        parentID: String? = "com.cyberpapiii.plug",
        parentVersion: String? = "20",
        programIdentifier: String? = nil,
        arguments: [String] = [],
        managedBy: String? = nil
    ) -> LaunchdJobRecord {
        LaunchdJobRecord(
            label: label,
            programURL: program,
            parentBundleIdentifier: parentID,
            parentBundleVersion: parentVersion,
            programIdentifier: programIdentifier,
            arguments: arguments,
            managedBy: managedBy
        )
    }

    private func assertUnknown(_ state: DaemonOwnershipState, programIdentifier: String) {
        guard case let .unknown(records) = state, let record = records.first else {
            XCTFail("Expected unknown launchd ownership, got \(state)")
            return
        }
        XCTAssertNil(record.programURL)
        XCTAssertEqual(record.programIdentifier, programIdentifier)
    }
}

private struct LegacyPlugProgramFixture: Decodable {
    let rules: Rules
    let recognized: [String]
    let homeRecognizedSuffixes: [String]
    let unrecognized: [String]

    struct Rules: Decodable {
        let exact: [String]
        let homeRelative: [String]
        let cellarRoots: [String]

        enum CodingKeys: String, CodingKey {
            case exact
            case homeRelative = "home_relative"
            case cellarRoots = "cellar_roots"
        }
    }

    enum CodingKeys: String, CodingKey {
        case rules
        case recognized
        case homeRecognizedSuffixes = "home_recognized_suffixes"
        case unrecognized
    }
}

/// A `launchctl print gui/501` whose services are `lines` of PID, status
/// and label.
private func domain(_ lines: String) -> String {
    let services = lines.split(separator: "\n").map { "\t\t\($0)\n" }.joined()
    return "gui/501 = {\n\ttype = gui\n\tservices = {\n\(services)\t}\n}\n"
}

/// The domain print names `labels`; each job's `print` gets `print`'s
/// answer for the label it asks about.
private func launchctl(
    listing labels: String,
    print: @escaping @Sendable (_ label: String) throws -> ProcessResult
) -> StubProcessRunner {
    StubProcessRunner { _, arguments in
        if arguments == ["print", "gui/501"] { return .exited(stdout: domain(labels)) }
        return try print(arguments.last?.split(separator: "/").last.map(String.init) ?? "")
    }
}

private extension ProcessResult {
    static let notFound = ProcessResult.exited(113, stderr: "Could not find service")
}

private func launchctlTimingOut(on slowLabel: String) -> StubProcessRunner {
    launchctl(listing: "123\t0\t\(slowLabel)\n124\t0\tcom.example.fast\n") { label in
        if label == slowLabel { throw ProcessRunnerError.timedOut }
        return .notFound
    }
}

private func launchctlFailing(list: Bool) -> StubProcessRunner {
    StubProcessRunner { _, arguments in
        if arguments == ["print", "gui/501"] {
            return list
                ? .exited(113, stderr: "list unavailable")
                : .exited(stdout: domain("123\t0\tcom.plug.daemon\n"))
        }
        return .exited(113, stderr: "print unavailable")
    }
}

private func launchctlRunning(_ programURL: URL) -> StubProcessRunner {
    launchctl(listing: "123\t0\tlocal.claude-rc.plug\n") { _ in
        .exited(stdout: "program = \(programURL.path)\nstate = running\n")
    }
}

private func launchctlWithAppServiceJob(
    parentBundleIdentifier: String,
    parentBundleVersion: String,
    arguments: [String]
) -> StubProcessRunner {
    let argumentLines = arguments.map { "    \($0)" }.joined(separator: "\n")
    return launchctl(listing: "123\t0\tcom.plug.daemon\n") { _ in
        .exited(stdout: """
        managed_by = com.apple.xpc.ServiceManagement
        program identifier = Contents/Resources/plug (mode: 2)
        parent bundle identifier = \(parentBundleIdentifier)
        parent bundle version = \(parentBundleVersion)
        arguments = {
        \(argumentLines)
        }
        state = running
        """)
    }
}

/// An unrelated label whose program is a recognized Plug, beside one in
/// Apple's namespace.
private func launchctlPrintingOneOwner() -> StubProcessRunner {
    launchctl(listing: "0\t0\tcom.apple.unrelated\n123\t0\tunexpected.owner\n") { label in
        guard label == "unexpected.owner" else { return .exited(stdout: "state = running\n") }
        return .exited(stdout: "program = /Users/me/.local/bin/plug\nstate = running\n")
    }
}

private final class LaunchdSymlinkFixture: @unchecked Sendable {
    let root: URL
    let legacyBinary: URL
    let shellLink: URL

    init() throws {
        root = FileManager.default.temporaryDirectory
            .appending(path: "plug-launchd-tests-\(UUID().uuidString)")
        legacyBinary = root.appending(path: ".cargo/bin/plug")
        shellLink = root.appending(path: ".local/bin/plug")
        try FileManager.default.createDirectory(
            at: legacyBinary.deletingLastPathComponent(),
            withIntermediateDirectories: true
        )
        try FileManager.default.createDirectory(
            at: shellLink.deletingLastPathComponent(),
            withIntermediateDirectories: true
        )
        FileManager.default.createFile(atPath: legacyBinary.path, contents: Data("legacy".utf8))
        try FileManager.default.createSymbolicLink(
            atPath: shellLink.path,
            withDestinationPath: legacyBinary.path
        )
    }

    func cleanup() {
        try? FileManager.default.removeItem(at: root)
    }
}
