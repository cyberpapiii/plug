import Foundation
import XCTest
@testable import Plug

final class LaunchdJobProbeTests: XCTestCase {
    private let record = LaunchdJobRecord(
        label: "local.plug.legacy",
        programURL: URL(fileURLWithPath: "/opt/homebrew/opt/plug/bin/plug"),
        parentBundleIdentifier: nil,
        parentBundleVersion: nil,
        loaded: true
    )

    func testReportsUnchangedWhenTheLabelStillNamesTheSameProgram() async throws {
        let runner = PrintingLaunchctlRunner(
            result: ProcessResult(
                status: 0,
                stdout: Data("program = /opt/homebrew/opt/plug/bin/plug\nstate = running\n".utf8),
                stderr: Data()
            )
        )

        let outcome = try await LaunchdJobProbe(runner: runner, userID: 501).verify(record)

        XCTAssertEqual(outcome, .unchanged)
        let calls = await runner.calls
        XCTAssertEqual(calls, [["print", "gui/501/local.plug.legacy"]])
    }

    func testReportsReplacedWhenTheLabelNowNamesADifferentProgram() async throws {
        let runner = PrintingLaunchctlRunner(
            result: ProcessResult(
                status: 0,
                stdout: Data("program = /usr/local/bin/something-else\nstate = running\n".utf8),
                stderr: Data()
            )
        )

        let outcome = try await LaunchdJobProbe(runner: runner, userID: 501).verify(record)

        XCTAssertEqual(outcome, .replaced(URL(fileURLWithPath: "/usr/local/bin/something-else")))
    }

    /// A job that reports no program path cannot be matched against the record
    /// that authorized removing it, so it is not removed.
    func testReportsReplacedWhenTheProgramPathCannotBeRead() async throws {
        let runner = PrintingLaunchctlRunner(
            result: ProcessResult(status: 0, stdout: Data("state = running\n".utf8), stderr: Data())
        )

        let outcome = try await LaunchdJobProbe(runner: runner, userID: 501).verify(record)

        XCTAssertEqual(outcome, .replaced(nil))
    }

    func testReportsVanishedWhenTheJobIsAlreadyGone() async throws {
        let runner = PrintingLaunchctlRunner(
            result: ProcessResult(
                status: 113,
                stdout: Data(),
                stderr: Data("Could not find service".utf8)
            )
        )

        let outcome = try await LaunchdJobProbe(runner: runner, userID: 501).verify(record)

        XCTAssertEqual(outcome, .vanished)
    }

    func testRejectsARecordWithNoProgramEvidence() async {
        let runner = PrintingLaunchctlRunner(
            result: ProcessResult(status: 0, stdout: Data(), stderr: Data())
        )
        let unproven = LaunchdJobRecord(
            label: "local.plug.legacy",
            programURL: nil,
            parentBundleIdentifier: nil,
            parentBundleVersion: nil,
            loaded: true
        )

        do {
            _ = try await LaunchdJobProbe(runner: runner, userID: 501).verify(unproven)
            XCTFail("Expected a failure")
        } catch {
            XCTAssertEqual(error as? DaemonServiceError, .invalidJobEvidence)
        }
        let calls = await runner.calls
        XCTAssertTrue(calls.isEmpty)
    }
}

private actor PrintingLaunchctlRunner: ProcessRunning {
    private(set) var calls: [[String]] = []
    private let result: ProcessResult

    init(result: ProcessResult) {
        self.result = result
    }

    func run(executable: URL, arguments: [String], timeout: Duration) async throws -> ProcessResult {
        calls.append(arguments)
        return result
    }
}
