import Foundation
import XCTest
@testable import Plug

final class AuthFlowServiceTests: XCTestCase {
    private let executable = URL(fileURLWithPath: "/usr/local/bin/plug")

    func testSignInInvokesTheLoginCommandForOneServer() async throws {
        let runner = StubProcessRunner(result: .exited())
        let service = AuthFlowService(runner: runner, executable: executable)

        try await service.signIn(server: "notion")

        let calls = await runner.calls
        let call = try XCTUnwrap(calls.first)
        XCTAssertEqual(call.executable, executable)
        XCTAssertEqual(call.arguments, ["auth", "login", "--server", "notion"])
    }

    func testSignOutInvokesTheLogoutCommandForOneServer() async throws {
        let runner = StubProcessRunner(result: .exited())
        let service = AuthFlowService(runner: runner, executable: executable)

        try await service.signOut(server: "notion")

        let calls = await runner.calls
        let call = try XCTUnwrap(calls.first)
        XCTAssertEqual(call.arguments, ["auth", "logout", "--server", "notion"])
    }

    /// A sign-in has to finish inside the CLI's own 120-second callback wait, so
    /// anything the app allows past that is headroom, never a shorter leash.
    func testAllowsMoreTimeThanTheCLICallbackWait() async throws {
        let runner = StubProcessRunner(result: .exited())
        let service = AuthFlowService(runner: runner, executable: executable)

        try await service.signIn(server: "notion")

        let calls = await runner.calls
        let call = try XCTUnwrap(calls.first)
        XCTAssertGreaterThan(call.timeout, .seconds(120))
    }

    func testReportsStandardErrorFromAFailedCommand() async {
        let runner = StubProcessRunner(result: .exited(1, stderr: "token endpoint refused\n"))
        let service = AuthFlowService(runner: runner, executable: executable)

        await assertFails(with: "token endpoint refused") {
            try await service.signIn(server: "notion")
        }
    }

    func testFallsBackToStandardOutputWhenStandardErrorIsEmpty() async {
        let runner = StubProcessRunner(result: .exited(2, stdout: "no such server: notion\n"))
        let service = AuthFlowService(runner: runner, executable: executable)

        await assertFails(with: "no such server: notion") {
            try await service.signIn(server: "notion")
        }
    }

    func testFallsBackToAReadableMessageWhenTheCommandSaysNothing() async {
        let runner = StubProcessRunner(result: .exited(1))
        let service = AuthFlowService(runner: runner, executable: executable)

        await assertFails(with: "Sign-in did not complete.") {
            try await service.signIn(server: "notion")
        }
    }

    func testReportsAReadableMessageWhenTheCommandTimesOut() async {
        let runner = StubProcessRunner(error: ProcessRunnerError.timedOut)
        let service = AuthFlowService(runner: runner, executable: executable)

        await assertFails(with: "Plug took too long to answer. Try again.") {
            try await service.signIn(server: "notion")
        }
    }

    func testFailsWhenTheBundledExecutableIsMissing() async {
        let runner = StubProcessRunner(result: .exited())
        let service = AuthFlowService(runner: runner, executable: nil)

        do {
            try await service.signIn(server: "notion")
            XCTFail("Expected a failure")
        } catch {
            XCTAssertTrue(error.localizedDescription.contains("Plug is damaged"))
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
