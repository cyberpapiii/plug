import Foundation
import XCTest
@testable import Plug

final class ClientRepairServiceTests: XCTestCase {
    private let canonicalExecutable = URL(
        fileURLWithPath: "/Applications/Plug.app/Contents/Resources/plug"
    )

    func testInspectReportsDriftFromReadOnlyRepairContract() async throws {
        let runner = StubProcessRunner(result: try .golden("repair_needed.json"))
        let service = ClientRepairService(runner: runner)

        let needsRepair = try await service.inspect(canonicalExecutable: canonicalExecutable)

        XCTAssertTrue(needsRepair)
        let calls = await runner.calls
        XCTAssertEqual(calls.map(\.arguments), [["repair", "--all", "--dry-run", "--output", "json"]])
        XCTAssertEqual(calls.first?.executable, canonicalExecutable)
    }

    func testInspectReportsNoDriftFromRepairContract() async throws {
        let runner = StubProcessRunner(result: try .golden("repair_clean.json"))
        let service = ClientRepairService(runner: runner)

        let needsRepair = try await service.inspect(canonicalExecutable: canonicalExecutable)

        XCTAssertFalse(needsRepair)
        let calls = await runner.calls
        XCTAssertEqual(calls.map(\.arguments), [["repair", "--all", "--dry-run", "--output", "json"]])
    }

    func testInspectRejectsNonzeroRepairStatus() async throws {
        let runner = StubProcessRunner(result: try .golden("repair_clean.json", status: 1))
        let service = ClientRepairService(runner: runner)

        do {
            _ = try await service.inspect(canonicalExecutable: canonicalExecutable)
            XCTFail("Expected command failure")
        } catch let error as ClientRepairError {
            XCTAssertEqual(error, .commandFailed(status: 1, stderr: ""))
        } catch {
            XCTFail("Unexpected error: \(error)")
        }
    }

    func testRepairAllInvokesVerifiedCanonicalExecutableAndDecodesStableReport() async throws {
        let runner = StubProcessRunner(result: try .golden("repair_clean.json"))
        let service = ClientRepairService(runner: runner)

        try await service.repairAll(canonicalExecutable: canonicalExecutable)

        let calls = await runner.calls
        XCTAssertEqual(calls.map(\.arguments), [
            ["repair", "--all", "--output", "json"],
        ])
        XCTAssertEqual(calls.first?.executable, canonicalExecutable)
    }

    func testNonzeroStatusPropagatesStderr() async {
        let runner = StubProcessRunner(result: .exited(23, stderr: "permission denied"))
        let service = ClientRepairService(runner: runner)

        do {
            try await service.repairAll(canonicalExecutable: canonicalExecutable)
            XCTFail("Expected command failure")
        } catch let error as ClientRepairError {
            XCTAssertEqual(error, .commandFailed(status: 23, stderr: "permission denied"))
        } catch {
            XCTFail("Unexpected error: \(error)")
        }
    }

    func testInspectBlocksMalformedOrIncompleteRepairJSON() async {
        let fixtures = [
            Data("not-json".utf8),
            Data(#"{"items":[{"changed":false}]}"#.utf8),
            Data(#"{"canonical_command":"/tmp/plug"}"#.utf8),
            // A report without the CLI's verdict cannot say no repair is needed.
            Data(#"{"canonical_command":"/tmp/plug","items":[]}"#.utf8),
        ]

        for fixture in fixtures {
            let runner = StubProcessRunner(result: ProcessResult(status: 0, stdout: fixture, stderr: Data()))
            let service = ClientRepairService(runner: runner)

            do {
                _ = try await service.inspect(canonicalExecutable: canonicalExecutable)
                XCTFail("Expected malformed repair JSON failure")
            } catch let error as ClientRepairError {
                XCTAssertEqual(error, .malformedOutput)
            } catch {
                XCTFail("Unexpected error: \(error)")
            }
        }
    }
}
