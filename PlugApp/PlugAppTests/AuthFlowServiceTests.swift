import Foundation
import XCTest
@testable import Plug

final class AuthFlowServiceTests: XCTestCase {
    private let executable = URL(fileURLWithPath: "/usr/local/bin/plug")

    func testSignInInvokesTheLoginCommandForOneServer() async throws {
        let runner = StubAuthRunner(result: ProcessResult(status: 0, stdout: Data(), stderr: Data()))
        let service = AuthFlowService(runner: runner, executable: executable)

        try await service.signIn(server: "notion")

        let calls = await runner.calls
        let call = try XCTUnwrap(calls.first)
        XCTAssertEqual(call.executable, executable)
        XCTAssertEqual(call.arguments, ["auth", "login", "--server", "notion"])
    }

    func testSignOutInvokesTheLogoutCommandForOneServer() async throws {
        let runner = StubAuthRunner(result: ProcessResult(status: 0, stdout: Data(), stderr: Data()))
        let service = AuthFlowService(runner: runner, executable: executable)

        try await service.signOut(server: "notion")

        let calls = await runner.calls
        let call = try XCTUnwrap(calls.first)
        XCTAssertEqual(call.arguments, ["auth", "logout", "--server", "notion"])
    }

    /// A sign-in has to finish inside the CLI's own 120-second callback wait, so
    /// anything the app allows past that is headroom, never a shorter leash.
    func testAllowsMoreTimeThanTheCLICallbackWait() async throws {
        let runner = StubAuthRunner(result: ProcessResult(status: 0, stdout: Data(), stderr: Data()))
        let service = AuthFlowService(runner: runner, executable: executable)

        try await service.signIn(server: "notion")

        let calls = await runner.calls
        let call = try XCTUnwrap(calls.first)
        XCTAssertGreaterThan(call.timeout, .seconds(120))
    }

    func testReportsStandardErrorFromAFailedCommand() async {
        let runner = StubAuthRunner(
            result: ProcessResult(status: 1, stdout: Data(), stderr: Data("token endpoint refused\n".utf8))
        )
        let service = AuthFlowService(runner: runner, executable: executable)

        await assertFails(with: "token endpoint refused") {
            try await service.signIn(server: "notion")
        }
    }

    func testFallsBackToStandardOutputWhenStandardErrorIsEmpty() async {
        let runner = StubAuthRunner(
            result: ProcessResult(status: 2, stdout: Data("no such server: notion\n".utf8), stderr: Data())
        )
        let service = AuthFlowService(runner: runner, executable: executable)

        await assertFails(with: "no such server: notion") {
            try await service.signIn(server: "notion")
        }
    }

    func testFallsBackToAReadableMessageWhenTheCommandSaysNothing() async {
        let runner = StubAuthRunner(result: ProcessResult(status: 1, stdout: Data(), stderr: Data()))
        let service = AuthFlowService(runner: runner, executable: executable)

        await assertFails(with: "Sign-in did not complete.") {
            try await service.signIn(server: "notion")
        }
    }

    func testReportsAReadableMessageWhenTheCommandTimesOut() async {
        let runner = StubAuthRunner(error: ProcessRunnerError.timedOut)
        let service = AuthFlowService(runner: runner, executable: executable)

        await assertFails(with: "The command did not finish in time and was stopped.") {
            try await service.signIn(server: "notion")
        }
    }

    func testFailsWhenTheBundledExecutableIsMissing() async {
        let runner = StubAuthRunner(result: ProcessResult(status: 0, stdout: Data(), stderr: Data()))
        let service = AuthFlowService(runner: runner, executable: nil)

        do {
            try await service.signIn(server: "notion")
            XCTFail("Expected a failure")
        } catch {
            XCTAssertTrue(error.localizedDescription.contains("bundled Plug service"))
        }
        let calls = await runner.calls
        XCTAssertTrue(calls.isEmpty)
    }

    private func assertFails(
        with message: String,
        file: StaticString = #filePath,
        line: UInt = #line,
        _ operation: () async throws -> Void
    ) async {
        do {
            try await operation()
            XCTFail("Expected a failure", file: file, line: line)
        } catch {
            XCTAssertEqual(error.localizedDescription, message, file: file, line: line)
        }
    }
}

private actor StubAuthRunner: ProcessRunning {
    struct Call: Sendable {
        let executable: URL
        let arguments: [String]
        let timeout: Duration
    }

    private(set) var calls: [Call] = []
    private let result: ProcessResult?
    private let error: (any Error)?

    init(result: ProcessResult) {
        self.result = result
        error = nil
    }

    init(error: any Error) {
        result = nil
        self.error = error
    }

    func run(executable: URL, arguments: [String], timeout: Duration) async throws -> ProcessResult {
        calls.append(Call(executable: executable, arguments: arguments, timeout: timeout))
        if let error { throw error }
        return result!
    }
}
