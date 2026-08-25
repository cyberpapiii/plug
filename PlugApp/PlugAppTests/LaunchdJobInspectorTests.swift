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
