import Foundation
import XCTest
@testable import Plug

final class ClientRepairServiceTests: XCTestCase {
    private let canonicalExecutable = URL(
        fileURLWithPath: "/Applications/Plug.app/Contents/Resources/plug"
    )

    func testInspectReportsDriftWhenRustReportsAChangedClient() async throws {
        let runner = RecordingRepairRunner(result: .success(.report(changed: [true, false])))
        let service = ClientRepairService(runner: runner)

        let needsRepair = try await service.inspect(canonicalExecutable: canonicalExecutable)

        XCTAssertTrue(needsRepair)
        let calls = await runner.calls
        XCTAssertEqual(calls, [
            .init(
                executable: canonicalExecutable,
                arguments: ["repair", "--all", "--output", "json"]
            ),
        ])
    }

    func testInspectReportsNoDriftWhenRustReportsNoChangedClients() async throws {
        let runner = RecordingRepairRunner(result: .success(.report(changed: [false, false])))
        let service = ClientRepairService(runner: runner)

        let needsRepair = try await service.inspect(canonicalExecutable: canonicalExecutable)

        XCTAssertFalse(needsRepair)
    }

    func testRepairAllInvokesVerifiedCanonicalExecutableAndDecodesStableReport() async throws {
        let runner = RecordingRepairRunner(
            result: .success(.report(changed: [true, false, false]))
        )
        let service = ClientRepairService(runner: runner)

        let result = try await service.repairAll(canonicalExecutable: canonicalExecutable)

        XCTAssertEqual(result, ClientRepairResult(examined: 3, repaired: 1, unchanged: 2))
        let calls = await runner.calls
        XCTAssertEqual(calls.map(\.arguments), [
            ["repair", "--all", "--output", "json"],
        ])
        XCTAssertEqual(calls.first?.executable, canonicalExecutable)
    }

    func testNonzeroStatusPropagatesStderr() async {
        let runner = RecordingRepairRunner(
            result: .success(
                ProcessResult(
                    status: 23,
                    stdout: Data(),
                    stderr: Data("permission denied".utf8)
                )
            )
        )
        let service = ClientRepairService(runner: runner)

        do {
            _ = try await service.repairAll(canonicalExecutable: canonicalExecutable)
            XCTFail("Expected command failure")
        } catch let error as ClientRepairError {
            XCTAssertEqual(error, .commandFailed(status: 23, stderr: "permission denied"))
        } catch {
            XCTFail("Unexpected error: \(error)")
        }
    }

    func testMalformedJSONBlocksRepair() async {
        let runner = RecordingRepairRunner(
            result: .success(
                ProcessResult(
                    status: 0,
                    stdout: Data("not-json".utf8),
                    stderr: Data()
                )
            )
        )
        let service = ClientRepairService(runner: runner)

        do {
            _ = try await service.repairAll(canonicalExecutable: canonicalExecutable)
            XCTFail("Expected malformed JSON failure")
        } catch let error as ClientRepairError {
            XCTAssertEqual(error, .malformedOutput)
        } catch {
            XCTFail("Unexpected error: \(error)")
        }
    }

    func testServiceDoesNotEditClientFiles() async throws {
        let root = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: root) }
        let clientFile = root.appending(path: "client.json")
        let original = "{\"mcpServers\":{\"plug\":{\"command\":\"legacy\"}}}"
        try Data(original.utf8).write(to: clientFile)

        let runner = RecordingRepairRunner(result: .success(.report(changed: [true])))
        let service = ClientRepairService(runner: runner)

        _ = try await service.repairAll(canonicalExecutable: canonicalExecutable)

        XCTAssertEqual(try String(contentsOf: clientFile, encoding: .utf8), original)
    }
}

private actor RecordingRepairRunner: ProcessRunning {
    struct Call: Equatable, Sendable {
        let executable: URL
        let arguments: [String]
    }

    private(set) var calls: [Call] = []
    private let result: Result<ProcessResult, Error>

    init(result: Result<ProcessResult, Error>) {
        self.result = result
    }

    func run(executable: URL, arguments: [String], timeout: Duration) async throws -> ProcessResult {
        calls.append(Call(executable: executable, arguments: arguments))
        return try result.get()
    }
}

private extension ProcessResult {
    static func report(changed: [Bool]) -> ProcessResult {
        let items = changed.enumerated().map { index, changed in
            "{\"target\":\"client-\(index)\",\"path\":\"/tmp/client-\(index).json\",\"disposition\":\"\(changed ? "RecognizedLegacy" : "Canonical")\",\"changed\":\(changed),\"message\":\"ok\"}"
        }.joined(separator: ",")
        let json = "{\"canonical_command\":\"/Applications/Plug.app/Contents/Resources/plug\",\"items\":[\(items)]}"
        return ProcessResult(status: 0, stdout: Data(json.utf8), stderr: Data())
    }
}
