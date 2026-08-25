import Foundation
import XCTest
@testable import Plug

final class ProcessRunnerTests: XCTestCase {
    private let runner = ProcessRunner()
    private let shell = URL(fileURLWithPath: "/bin/sh")

    func testCapturesStandardOutputAndStandardError() async throws {
        let result = try await runner.run(
            executable: shell,
            arguments: ["-c", "printf 'hello'; printf 'warning' >&2"],
            timeout: .seconds(2)
        )

        XCTAssertEqual(result.status, 0)
        XCTAssertEqual(String(decoding: result.stdout, as: UTF8.self), "hello")
        XCTAssertEqual(String(decoding: result.stderr, as: UTF8.self), "warning")
    }

    func testPreservesNonzeroExitStatus() async throws {
        let result = try await runner.run(
            executable: shell,
            arguments: ["-c", "printf 'failed' >&2; exit 23"],
            timeout: .seconds(2)
        )

        XCTAssertEqual(result.status, 23)
        XCTAssertEqual(String(decoding: result.stderr, as: UTF8.self), "failed")
    }

    func testTerminatesProcessWhenTimeoutExpires() async {
        let clock = ContinuousClock()
        let started = clock.now

        do {
            _ = try await runner.run(
                executable: URL(fileURLWithPath: "/bin/sleep"),
                arguments: ["5"],
                timeout: .milliseconds(100)
            )
            XCTFail("Expected timeout")
        } catch {
            XCTAssertEqual(error as? ProcessRunnerError, .timedOut)
        }

        XCTAssertLessThan(started.duration(to: clock.now), .seconds(2))
    }

    func testForceTerminatesProcessThatIgnoresGracefulTermination() async {
        let clock = ContinuousClock()
        let started = clock.now

        do {
            _ = try await runner.run(
                executable: URL(fileURLWithPath: "/usr/bin/python3"),
                arguments: [
                    "-c",
                    "import signal,time; signal.signal(signal.SIGTERM, signal.SIG_IGN); time.sleep(4)",
                ],
                timeout: .seconds(1)
            )
            XCTFail("Expected timeout")
        } catch {
            XCTAssertEqual(error as? ProcessRunnerError, .timedOut)
        }

        XCTAssertLessThan(started.duration(to: clock.now), .seconds(2))
    }

    func testTimeoutKillsDescendantThatRetainsOutputPipes() async {
        let clock = ContinuousClock()
        let started = clock.now

        do {
            _ = try await runner.run(
                executable: shell,
                arguments: ["-c", "sleep 4 &"],
                timeout: .milliseconds(100)
            )
            XCTFail("Expected timeout")
        } catch {
            XCTAssertEqual(error as? ProcessRunnerError, .timedOut)
        }

        XCTAssertLessThan(started.duration(to: clock.now), .seconds(1))
    }

    @MainActor
    func testRunDoesNotBlockMainActor() async throws {
        let clock = ContinuousClock()
        let started = clock.now
        let running = Task {
            try await runner.run(
                executable: shell,
                arguments: ["-c", "sleep 0.3; printf 'done'"],
                timeout: .seconds(2)
            )
        }

        try await Task.sleep(for: .milliseconds(30))
        XCTAssertLessThan(started.duration(to: clock.now), .milliseconds(150))

        let result = try await running.value
        XCTAssertEqual(String(decoding: result.stdout, as: UTF8.self), "done")
    }
}
