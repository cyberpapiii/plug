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
            // An ignored signal stays ignored across exec, so `sleep` ignores
            // SIGTERM too.
            _ = try await runner.run(
                executable: shell,
                arguments: ["-c", "trap '' TERM; sleep 4"],
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

    /// A folder for files a child and the test signal each other with.
    private func signals() throws -> URL {
        let directory = URL(fileURLWithPath: "/tmp/plug-runner-\(UUID().uuidString.prefix(8))")
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        addTeardownBlock { try? FileManager.default.removeItem(at: directory) }
        return directory
    }

    /// Waits for `condition`, polling, for at most ten seconds.
    @MainActor
    private func eventually(_ condition: () -> Bool) async -> Bool {
        for _ in 0..<1_000 {
            if condition() { return true }
            try? await Task.sleep(for: .milliseconds(10))
        }
        return condition()
    }

    /// Cancelling the call stops the child's process group, the way a timeout
    /// does.
    @MainActor
    func testCancellingTheCallStopsTheProcessGroup() async throws {
        let folder = try signals()
        let pidFile = folder.appending(path: "pid")
        let running = Task {
            try await runner.run(
                executable: shell,
                arguments: ["-c", "sleep 30 & echo $! > '\(pidFile.path)'; wait"],
                timeout: .seconds(60)
            )
        }
        let started = await eventually {
            (try? String(contentsOf: pidFile, encoding: .utf8))?.hasSuffix("\n") == true
        }
        XCTAssertTrue(started, "the child started")
        let pid = try XCTUnwrap(Int32(String(contentsOf: pidFile, encoding: .utf8).trimmingCharacters(in: .whitespacesAndNewlines)))
        running.cancel()

        do {
            _ = try await running.value
            XCTFail("Expected cancellation")
        } catch {
            XCTAssertTrue(error is CancellationError, "\(error)")
        }
        let stopped = await eventually { kill(pid, 0) != 0 }
        XCTAssertTrue(stopped, "the child's own child is gone too")
    }

    @MainActor
    func testACallCancelledBeforeItStartsDoesNotRun() async {
        let clock = ContinuousClock()
        let started = clock.now
        let running = Task {
            withUnsafeCurrentTask { $0?.cancel() }
            return try await runner.run(
                executable: URL(fileURLWithPath: "/bin/sleep"), arguments: ["5"], timeout: .seconds(10)
            )
        }
        do {
            _ = try await running.value
            XCTFail("Expected cancellation")
        } catch {
            XCTAssertTrue(error is CancellationError, "\(error)")
        }
        XCTAssertLessThan(started.duration(to: clock.now), .seconds(2))
    }

    /// A child that writes past the 64 KB pipe buffer blocks on the write until
    /// the parent reads. A runner that waits for exit before draining its pipes
    /// deadlocks here and never returns.
    func testCapturesOutputLargerThanThePipeBuffer() async throws {
        let size = 512 * 1024
        let result = try await runner.run(
            executable: shell,
            arguments: [
                "-c",
                "head -c \(size) /dev/zero | tr '\\0' o; head -c \(size) /dev/zero | tr '\\0' e >&2",
            ],
            timeout: .seconds(10)
        )

        XCTAssertEqual(result.status, 0)
        XCTAssertEqual(result.stdout.count, size)
        XCTAssertEqual(result.stderr.count, size)
    }

    /// The child finishes only once the main actor has answered it. A run
    /// that held the main actor until the child exited would never let it.
    @MainActor
    func testRunDoesNotBlockMainActor() async throws {
        let folder = try signals()
        let started = folder.appending(path: "started")
        let answered = folder.appending(path: "answered")
        let running = Task {
            try await runner.run(
                executable: shell,
                arguments: [
                    "-c",
                    "touch '\(started.path)'; while [ ! -e '\(answered.path)' ]; do sleep 0.01; done; printf done",
                ],
                timeout: .seconds(60)
            )
        }

        let childStarted = await eventually { FileManager.default.fileExists(atPath: started.path) }
        XCTAssertTrue(childStarted)
        FileManager.default.createFile(atPath: answered.path, contents: nil)

        let result = try await running.value
        XCTAssertEqual(String(decoding: result.stdout, as: UTF8.self), "done")
    }
}
