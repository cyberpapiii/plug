import Foundation
import XCTest
@testable import Plug

final class LaunchdJobInspectorTests: XCTestCase {
    private let canonical = VerifiedAppInstallation(
        bundleURL: URL(fileURLWithPath: "/Applications/Plug.app"),
        executableURL: URL(fileURLWithPath: "/Applications/Plug.app/Contents/Resources/plug"),
        appVersion: "0.7.0",
        buildVersion: "20",
        embeddedVersion: "0.7.0",
        teamID: "HJF7LN64XX"
    )

    func testClassifiesCurrentAndStaleAppManagedJobs() async throws {
        let current = record(label: "com.plug.daemon", program: canonical.executableURL, parentVersion: "20")
        let currentState = try await LaunchdJobInspector(records: { [current] }).daemonJobs(canonical: canonical, recognizedLegacyPaths: [])
        XCTAssertEqual(currentState, .appManagedCurrent(current))

        let stale = record(label: "com.plug.daemon", program: canonical.executableURL, parentVersion: "19")
        let staleState = try await LaunchdJobInspector(records: { [stale] }).daemonJobs(canonical: canonical, recognizedLegacyPaths: [])
        XCTAssertEqual(staleState, .appManagedStale(stale))
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

    func testBroadEnumerationFindsPlugProgramBehindUnrelatedLabel() async throws {
        let runner = RecordingLaunchctlRunner()
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
        let printedLabels = await runner.printedLabels
        XCTAssertEqual(printedLabels.sorted(), ["com.apple.unrelated", "unexpected.owner"])
    }

    func testLaunchctlListFailureIsPropagatedInsteadOfReportingUnmanaged() async {
        let inspector = LaunchdJobInspector(
            runner: FailingLaunchctlRunner(failure: .list),
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
            runner: FailingLaunchctlRunner(failure: .print),
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

    func testLaunchdProgramSymlinkResolvesToRecognizedLegacyBinary() async throws {
        let fixture = try LaunchdSymlinkFixture()
        defer { fixture.cleanup() }
        let runner = SymlinkLaunchctlRunner(programURL: fixture.shellLink)
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
        program: URL,
        parentID: String? = "com.cyberpapiii.plug",
        parentVersion: String? = "20"
    ) -> LaunchdJobRecord {
        LaunchdJobRecord(
            label: label,
            programURL: program,
            parentBundleIdentifier: parentID,
            parentBundleVersion: parentVersion,
            loaded: true
        )
    }
}

private actor FailingLaunchctlRunner: ProcessRunning {
    enum Failure: Equatable, Sendable {
        case list
        case print
    }

    let failure: Failure

    init(failure: Failure) {
        self.failure = failure
    }

    func run(executable: URL, arguments: [String], timeout: Duration) async throws -> ProcessResult {
        if arguments == ["list"] {
            if failure == .list {
                return ProcessResult(status: 113, stdout: Data(), stderr: Data("list unavailable".utf8))
            }
            return ProcessResult(
                status: 0,
                stdout: Data("PID\tStatus\tLabel\n123\t0\tcom.plug.daemon\n".utf8),
                stderr: Data()
            )
        }
        return ProcessResult(status: 113, stdout: Data(), stderr: Data("print unavailable".utf8))
    }
}

private actor SymlinkLaunchctlRunner: ProcessRunning {
    let programURL: URL

    init(programURL: URL) {
        self.programURL = programURL
    }

    func run(executable: URL, arguments: [String], timeout: Duration) async throws -> ProcessResult {
        if arguments == ["list"] {
            return ProcessResult(
                status: 0,
                stdout: Data("PID\tStatus\tLabel\n123\t0\tlocal.claude-rc.plug\n".utf8),
                stderr: Data()
            )
        }
        let output = "program = \(programURL.path)\nstate = running\n"
        return ProcessResult(status: 0, stdout: Data(output.utf8), stderr: Data())
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

private actor RecordingLaunchctlRunner: ProcessRunning {
    private(set) var printedLabels: [String] = []

    func run(executable: URL, arguments: [String], timeout: Duration) async throws -> ProcessResult {
        if arguments == ["list"] {
            let output = "PID\tStatus\tLabel\n-\t0\tcom.apple.unrelated\n123\t0\tunexpected.owner\n"
            return ProcessResult(status: 0, stdout: Data(output.utf8), stderr: Data())
        }

        let label = arguments.last?.split(separator: "/").last.map(String.init) ?? ""
        printedLabels.append(label)
        if label == "unexpected.owner" {
            let output = """
            program = /Users/me/.local/bin/plug
            state = running
            """
            return ProcessResult(status: 0, stdout: Data(output.utf8), stderr: Data())
        }
        return ProcessResult(status: 0, stdout: Data("state = running\n".utf8), stderr: Data())
    }
}
